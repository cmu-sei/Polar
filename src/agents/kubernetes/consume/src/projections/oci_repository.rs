use kube_common::KIND_OCI_REPOSITORY;
use kube_common::flux::oci_repositories::OciRepository;
use neo4rs::BoltType;
use polar::cassini::CassiniClient;
use polar::graph::controller::{
    GraphController, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property,
};
use polar::graph::nodes::kube::KubeNodeKey;
use ractor::ActorProcessingErr;
use tracing::debug;

use crate::supervisor::{EmitDecision, ProjectionCache};

use super::{
    GraphOperable, now_ms, opt_json, project_deletion_state, rfc3339_ms, state_signature, ts,
};

// TODO: Create a node linking this OCI registry to a canonical representation.
// Something like - FluxOCIRepository -[:RECONCILES_FROM]->(OCIRegistry)
impl GraphOperable for OciRepository {
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

        let repo_key = KubeNodeKey::FluxOciRepository {
            uid: uid.clone(),
            cluster_uid: cluster_uid.to_string(),
        };

        // ---- Anchor node ----
        //
        // spec.url is the only mandatory field that meaningfully identifies
        // this source — it's the registry address being polled.

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: repo_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name.clone())),
                Property("url".into(), GraphValue::String(self.spec.url.clone())),
                ts("observed_at", now_ms()),
            ],
        }))?;

        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
            from: repo_key.clone().into_key(),
            rel_type: "IN_NAMESPACE".into(),
            to: KubeNodeKey::Namespace {
                name: namespace.clone(),
                cluster_uid: cluster_uid.to_string(),
            }
            .into_key(),
            props: Vec::new(),
        }))?;

        // ---- State ----
        //
        // We only emit a state node when status.artifact is present — the
        // resource exists in the API before its first successful sync, and
        // there is nothing meaningful to record until Flux has actually
        // resolved the artifact.

        let Some(status) = self.status else {
            return Ok(());
        };

        let ready_condition = status
            .conditions
            .as_ref()
            .and_then(|conds| conds.iter().find(|c| c.type_ == "Ready"))
            .map(|c| c.reason.clone())
            .unwrap_or_else(|| "Unknown".into());

        let Some(artifact) = status.artifact else {
            return Ok(());
        };

        // Use artifact.last_update_time as valid_from so the state timeline
        // reflects when Flux actually resolved the artifact, not when the
        // observer happened to receive the event. The CRD carries it as an
        // RFC3339 string; a parse failure falls back to observation time,
        // which keeps the timeline monotonic rather than silently dropping
        // the state.
        let valid_from_ms = rfc3339_ms(&artifact.last_update_time).unwrap_or_else(now_ms);

        let state_key = KubeNodeKey::FluxOciRepositoryState {
            uid: uid.clone(),
            valid_from: valid_from_ms.to_string(),
            cluster_uid: cluster_uid.to_string(),
        };

        let meaningful_props = vec![
            Property("digest".into(), GraphValue::String(artifact.digest.clone())),
            Property(
                "revision".into(),
                GraphValue::String(artifact.revision.clone()),
            ),
            Property("ready_reason".into(), GraphValue::String(ready_condition)),
            // OCI annotations — org.opencontainers.image.revision etc.
            // Serialized wholesale; the processor graph queries can
            // extract individual annotation values as needed.
            Property("annotations".into(), opt_json(&artifact.metadata)),
        ];
        let signature = state_signature(&meaningful_props);

        match cache.should_emit(
            KIND_OCI_REPOSITORY.to_string(),
            cluster_uid.to_string(),
            uid.clone(),
            signature,
            valid_from_ms.to_string(),
        ) {
            EmitDecision::Suppress => {
                debug!(
                    "OCIRepository {uid} state unchanged since last observation, suppressing write"
                );
            }
            EmitDecision::Emit => {
                let mut props = meaningful_props;
                props.push(ts("valid_from", valid_from_ms));
                props.push(ts("observed_at", now_ms()));

                graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                    resource_key: repo_key.clone().into_key(),
                    state_type_key: KubeNodeKey::State.into_key(),
                    state_instance_key: state_key.into_key(),
                    state_instance_props: props,
                }))?;
            }
        }

        // ---- RECONCILED edge to the canonical OCIArtifact ----
        //
        // Anchored on THIS repository's identity properties (which this impl
        // writes above), not a label-wide scan of FluxOCIRepositoryState by
        // digest — the previous form matched every state in the graph per
        // event, which is O(all states) with no index (constraints are
        // deferred) and merged edges for unrelated repositories that had
        // ever seen the same digest.
        //
        // MERGE is a no-op when the OCIArtifact doesn't exist yet (the MATCH
        // yields nothing); the next observation retries. Digest format must
        // match what the OCI resolver writes on OCIArtifact.digest —
        // "sha256:<hex>" — verify once against real data.

        // NOTE: this is a natural-key match, not a KubeNodeKey::cypher_match
        // -- see the TODO on the Kustomization impl below for why. {name,
        // cluster_uid} alone is not unique across namespaces in the same
        // cluster (two OCIRepositories can share a name in different
        // namespaces), so the match routes through the IN_NAMESPACE edge
        // rather than a namespace property -- this anchor no longer carries
        // one; see the edge written just above in this function.
        graph.cast(GraphControllerMsg::Op(GraphOp::RawQuery {
            cypher: "
                MATCH (repo:FluxOCIRepository {name: $name, cluster_uid: $cluster_uid})
                    -[:IN_NAMESPACE]->(:Namespace {name: $namespace, cluster_uid: $cluster_uid})
                MATCH (oci:OCIArtifact {digest: $digest})
                MERGE (repo)-[:RECONCILED]->(oci)
            "
            .into(),
            params: vec![
                ("name".into(), BoltType::String(name.into())),
                ("namespace".into(), BoltType::String(namespace.into())),
                (
                    "cluster_uid".into(),
                    BoltType::String(cluster_uid.to_string().into()),
                ),
                (
                    "digest".into(),
                    BoltType::String(artifact.digest.clone().into()),
                ),
            ],
        }))?;

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
            KubeNodeKey::FluxOciRepository {
                uid: uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            },
            KubeNodeKey::FluxOciRepositoryState {
                uid: uid.clone(),
                valid_from: now.to_string(),
                cluster_uid: cluster_uid.to_string(),
            },
            now,
        )?;

        cache.evict(
            KIND_OCI_REPOSITORY.to_string(),
            cluster_uid.to_string(),
            &uid,
        );

        Ok(())
    }
}
