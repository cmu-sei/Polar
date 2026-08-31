use k8s_openapi::api::core::v1::Namespace;
use polar::cassini::CassiniClient;
use polar::graph::controller::{
    GraphController, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property,
};
use polar::graph::nodes::kube::KubeNodeKey;
use ractor::ActorProcessingErr;
use tracing::debug;

use crate::supervisor::{EmitDecision, ProjectionCache};

use super::{GraphOperable, now_ms, project_deletion_state, state_signature, ts};

impl GraphOperable for Namespace {
    fn project_into_graph(
        self,
        graph: &GraphController,
        _tcp_client: &dyn CassiniClient,
        cluster_uid: &str,
        cache: &mut ProjectionCache,
    ) -> Result<(), ActorProcessingErr> {
        let Some(name) = self.metadata.name.clone() else {
            return Ok(());
        };

        let ns_key = KubeNodeKey::Namespace {
            name: name.clone(),
            cluster_uid: cluster_uid.to_string(),
        };

        // ---- Anchor node ----
        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: ns_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name.clone())),
                ts("observed_at", now_ms()),
            ],
        }))?;

        // ---- State ----
        //
        // Namespace's status carries only `phase` (Active/Terminating) --
        // no attested transition timestamp the way Flux resources have
        // last_update_time, so valid_from falls back to observation time,
        // same as Job/Deployment/ReplicaSet/Pod do for the same reason.
        let phase = self
            .status
            .as_ref()
            .and_then(|s| s.phase.clone())
            .unwrap_or_else(|| "Unknown".into());

        let valid_from_ms = now_ms();

        let state_key = KubeNodeKey::NamespaceState {
            name: name.clone(),
            valid_from: valid_from_ms.to_string(),
            cluster_uid: cluster_uid.to_string(),
        };

        let meaningful_props = vec![Property("phase".into(), GraphValue::String(phase))];
        let signature = state_signature(&meaningful_props);

        match cache.should_emit(
            "Namespace".to_string(),
            cluster_uid.to_string(),
            name,
            signature,
            valid_from_ms.to_string(),
        ) {
            EmitDecision::Suppress => {
                debug!("Namespace state unchanged since last observation, suppressing write");
            }
            EmitDecision::Emit => {
                let mut props = meaningful_props;
                props.push(ts("valid_from", valid_from_ms));
                props.push(ts("observed_at", now_ms()));

                graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                    resource_key: ns_key.into_key(),
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
        let Some(name) = self.metadata.name.clone() else {
            return Ok(());
        };
        let now = now_ms();

        project_deletion_state(
            graph,
            KubeNodeKey::Namespace {
                name: name.clone(),
                cluster_uid: cluster_uid.to_string(),
            },
            KubeNodeKey::NamespaceState {
                name,
                valid_from: now.to_string(),
                cluster_uid: cluster_uid.to_string(),
            },
            now,
        )
    }
}
