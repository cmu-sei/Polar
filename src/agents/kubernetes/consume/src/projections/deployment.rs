use k8s_openapi::api::apps::v1::Deployment;
use polar::cassini::CassiniClient;
use polar::graph::controller::{
    GraphController, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, NULL_FIELD, Property,
};
use polar::graph::nodes::kube::KubeNodeKey;
use ractor::ActorProcessingErr;
use tracing::debug;

use crate::supervisor::{EmitDecision, ProjectionCache};

use super::{
    GraphOperable, handle_owner_refs, k8s_time_ms, now_ms, project_deletion_state, state_signature,
    ts,
};

impl GraphOperable for Deployment {
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

        let status = self.status.unwrap_or_default();

        let available = status.available_replicas.unwrap_or(0);
        let updated = status.updated_replicas.unwrap_or(0);
        let unavailable = status.unavailable_replicas.unwrap_or(0);

        let progressing_condition = status
            .conditions
            .as_ref()
            .and_then(|conds| {
                conds
                    .iter()
                    .find(|c| c.type_ == "Progressing")
                    .map(|c| c.status.clone())
            })
            .unwrap_or_else(|| NULL_FIELD.into());

        let available_condition = status
            .conditions
            .as_ref()
            .and_then(|conds| {
                conds
                    .iter()
                    .find(|c| c.type_ == "Available")
                    .map(|c| c.status.clone())
            })
            .unwrap_or_else(|| NULL_FIELD.into());

        // Prefer the k8s-attested transition time of the most recently
        // transitioned condition as valid_from; fall back to observation
        // time when no conditions exist yet.
        let valid_from_ms = status
            .conditions
            .as_ref()
            .and_then(|conds| {
                conds
                    .iter()
                    .filter_map(|c| c.last_transition_time.as_ref().map(k8s_time_ms))
                    .max()
            })
            .unwrap_or_else(now_ms);

        let deployment_key = KubeNodeKey::Deployment {
            uid: uid.clone(),
            cluster_uid: cluster_uid.to_string(),
        };

        // ---- Anchor node ----

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: deployment_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name)),
                ts("observed_at", now_ms()),
            ],
        }))?;

        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
            from: deployment_key.clone().into_key(),
            rel_type: "IN_NAMESPACE".into(),
            to: KubeNodeKey::Namespace {
                name: namespace,
                cluster_uid: cluster_uid.to_string(),
            }
            .into_key(),
            props: Vec::new(),
        }))?;

        // ---- Owner refs ----

        if let Some(owners) = self.metadata.owner_references {
            handle_owner_refs(&owners, deployment_key.clone(), graph, cluster_uid)?;
        }

        // ---- Immutable DeploymentState ----

        let state_key = KubeNodeKey::DeploymentState {
            uid: uid.clone(),
            valid_from: valid_from_ms.to_string(),
            cluster_uid: cluster_uid.to_string(),
        };

        let meaningful_props = vec![
            Property(
                "available_replicas".into(),
                GraphValue::I64(available as i64),
            ),
            Property("updated_replicas".into(), GraphValue::I64(updated as i64)),
            Property(
                "unavailable_replicas".into(),
                GraphValue::I64(unavailable as i64),
            ),
            Property(
                "progressing_condition".into(),
                GraphValue::String(progressing_condition),
            ),
            Property(
                "available_condition".into(),
                GraphValue::String(available_condition),
            ),
        ];
        let signature = state_signature(&meaningful_props);

        match cache.should_emit(
            "Deployment".to_string(),
            cluster_uid.to_string(),
            uid.clone(),
            signature,
            valid_from_ms.to_string(),
        ) {
            EmitDecision::Suppress => {
                debug!(
                    "Deployment {uid} state unchanged since last observation, suppressing write"
                );
            }
            EmitDecision::Emit => {
                let mut props = meaningful_props;
                props.push(ts("valid_from", valid_from_ms));
                props.push(ts("observed_at", now_ms()));

                graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                    resource_key: deployment_key.clone().into_key(),
                    state_type_key: KubeNodeKey::State.into_key(),
                    state_instance_key: state_key.into_key(),
                    state_instance_props: props,
                }))?;
            }
        }

        Ok(())
    }

    fn project_delete(
        self,
        graph: &GraphController,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let now = now_ms();

        // The previous implementation wrote a final ordinary state (replica
        // counts, conditions) with no "Deleted" phase — so a deleted
        // Deployment was indistinguishable from a live one that happened to
        // stop receiving events. Deletion is now recorded as the terminal
        // phase, and no replica counts are fabricated for it.
        project_deletion_state(
            graph,
            KubeNodeKey::Deployment {
                uid: uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            },
            KubeNodeKey::DeploymentState {
                cluster_uid: cluster_uid.to_string(),
                uid,
                valid_from: now.to_string(),
            },
            now,
        )
    }
}
