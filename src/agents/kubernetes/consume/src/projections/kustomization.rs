use kube_common::KIND_KUSTOMIZATION;
use kube_common::flux::kustomization::Kustomization;
use neo4rs::BoltType;
use polar::cassini::CassiniClient;
use polar::graph::controller::{
    GraphController, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property,
};
use polar::graph::nodes::kube::KubeNodeKey;
use ractor::ActorProcessingErr;
use tracing::debug;

use crate::supervisor::{EmitDecision, ProjectionCache};

use super::{GraphOperable, now_ms, opt_string, project_deletion_state, state_signature, ts};

impl GraphOperable for Kustomization {
    fn project_into_graph(
        self,
        graph: &GraphController,
        _tcp_client: &dyn CassiniClient,
        cluster_uid: &str,
        cache: &mut ProjectionCache,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let name = self.metadata.name.clone().unwrap_or_default();
        let namespace = self
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());

        let ks_key = KubeNodeKey::FluxKustomization {
            uid: uid.clone(),
            cluster_uid: cluster_uid.to_string(),
        };

        // ---- Anchor node ----
        //
        // source_ref is serialized wholesale so the graph retains the full
        // pointer — kind, name, namespace — without requiring a separate
        // edge resolution at write time. The RECONCILES_FROM edge (see TODO
        // below) will handle the structural link; this is for query
        // convenience.

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: ks_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name.clone())),
                Property(
                    "source_ref".into(),
                    GraphValue::String(
                        serde_json::to_string(&self.spec.source_ref)
                            .unwrap_or_else(|_| "{}".into()),
                    ),
                ),
                ts("observed_at", now_ms()),
            ],
        }))?;

        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
            from: ks_key.clone().into_key(),
            rel_type: "IN_NAMESPACE".into(),
            to: KubeNodeKey::Namespace {
                name: namespace.clone(),
                cluster_uid: cluster_uid.to_string(),
            }
            .into_key(),
            props: Vec::new(),
        }))?;

        // ---- RECONCILES_FROM edge: honest TODO, not dead code ----
        //
        // Blocked, precisely: KubeNodeKey::FluxOciRepository is keyed by
        // uid, but source_ref only gives (kind, name, namespace) — with the
        // Flux-specified default of the Kustomization's own namespace when
        // source_ref.namespace is None. Building the edge therefore needs
        // either (a) a natural-key RawQuery matching FluxOCIRepository on
        // {name, namespace} (the properties this consumer writes), or (b) a
        // natural-key KubeNodeKey variant. Option (a) is a four-line
        // RawQuery mirroring the RECONCILED merge above. Deprioritized: the
        // provenance chain no longer routes through Flux (runtime join is
        // PodContainer-USES_IMAGE->OCIArtifact), so this edge is
        // explanatory, not load-bearing. GitRepository and Bucket sources
        // remain intentionally out of scope (see spike #201 notes).

        // ---- State ----
        //
        // Guard on status presence — Kustomizations start life with no status.

        let Some(status) = self.status else {
            return Ok(());
        };

        // Extract the Ready condition. last_transition_time is the authoritative
        // valid_from — it's when kustomize-controller actually finished, not
        // when we observed it.
        let ready_condition = status
            .conditions
            .as_ref()
            .and_then(|conds| conds.iter().find(|c| c.type_ == "Ready"));

        let valid_from_ms = ready_condition
            .map(|c| c.last_transition_time.0.as_millisecond())
            .unwrap_or_else(now_ms);

        let ready_reason = ready_condition
            .map(|c| c.reason.clone())
            .unwrap_or_else(|| "Unknown".into());

        // Only record state when we have at least one of the revision fields.
        // A Kustomization that has never successfully reconciled has nothing
        // meaningful to write.
        if status.last_applied_revision.is_none() && status.last_applied_origin_revision.is_none() {
            return Ok(());
        }

        let state_key = KubeNodeKey::FluxKustomizationState {
            uid: uid.clone(),
            valid_from: valid_from_ms.to_string(),
            cluster_uid: cluster_uid.to_string(),
        };

        let meaningful_props = vec![
            // last_applied_revision is the OCI content digest — the join
            // key that links this reconciliation back to what Flux fetched
            // from the registry.
            Property(
                "last_applied_revision".into(),
                opt_string(&status.last_applied_revision),
            ),
            // last_applied_origin_revision carries the value of
            // org.opencontainers.image.revision from the OCI annotations
            // — the git ref or commit SHA embedded by the pipeline.
            // This is the join key back to SCM events.
            Property(
                "last_applied_origin_revision".into(),
                opt_string(&status.last_applied_origin_revision),
            ),
            Property("ready_reason".into(), GraphValue::String(ready_reason)),
        ];
        let signature = state_signature(&meaningful_props);

        match cache.should_emit(
            KIND_KUSTOMIZATION.to_string(),
            cluster_uid.to_string(),
            uid.clone(),
            signature,
            valid_from_ms.to_string(),
        ) {
            EmitDecision::Suppress => {
                debug!(
                    "Kustomization {uid} state unchanged since last observation, suppressing write"
                );
            }
            EmitDecision::Emit => {
                let mut props = meaningful_props;
                props.push(ts("valid_from", valid_from_ms));
                props.push(ts("observed_at", now_ms()));

                graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                    resource_key: ks_key.clone().into_key(),
                    state_type_key: KubeNodeKey::State.into_key(),
                    state_instance_key: state_key.into_key(),
                    state_instance_props: props,
                }))?;
            }
        }

        // ---- DEPLOYED edge to the canonical OCIArtifact ----
        //
        // Anchored on THIS Kustomization's identity properties instead of
        // the previous label-wide `ENDS WITH` scan across every
        // FluxKustomizationState in the graph. The digest is extracted from
        // the Flux revision format `<tag>@sha256:<hex>` (bare `sha256:<hex>`
        // passes through the unwrap_or) and equality-matched — the suffix
        // match is gone entirely, along with its false-positive surface.

        if let Some(ref revision) = status.last_applied_revision {
            let digest = revision
                .split('@')
                .nth(1)
                .unwrap_or(revision.as_str())
                .to_string();

            // NOTE: natural-key match, same as FluxOCIRepository's RECONCILED
            // query above -- {name, cluster_uid} alone is not unique across
            // namespaces in the same cluster, so the match routes through
            // the IN_NAMESPACE edge rather than a namespace property; this
            // anchor no longer carries one, see the edge written earlier in
            // this function.
            graph.cast(GraphControllerMsg::Op(GraphOp::RawQuery {
                cypher: "
                    MATCH (ks:FluxKustomization {name: $name, cluster_uid: $cluster_uid})
                        -[:IN_NAMESPACE]->(:Namespace {name: $namespace, cluster_uid: $cluster_uid})
                    MATCH (oci:OCIArtifact {digest: $digest})
                    MERGE (ks)-[:DEPLOYED]->(oci)
                "
                .into(),
                params: vec![
                    ("name".into(), BoltType::String(name.into())),
                    ("namespace".into(), BoltType::String(namespace.into())),
                    (
                        "cluster_uid".into(),
                        BoltType::String(cluster_uid.to_string().into()),
                    ),
                    ("digest".into(), BoltType::String(digest.into())),
                ],
            }))?;
        }
        Ok(())
    }

    fn project_delete(
        self,
        graph: &GraphController,
        cluster_uid: &str,
        cache: &mut ProjectionCache,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let now = now_ms();

        project_deletion_state(
            graph,
            KubeNodeKey::FluxKustomization {
                uid: uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            },
            KubeNodeKey::FluxKustomizationState {
                uid: uid.clone(),
                valid_from: now.to_string(),
                cluster_uid: cluster_uid.to_string(),
            },
            now,
        )?;

        cache.evict(
            KIND_KUSTOMIZATION.to_string(),
            cluster_uid.to_string(),
            &uid,
        );

        Ok(())
    }
}
