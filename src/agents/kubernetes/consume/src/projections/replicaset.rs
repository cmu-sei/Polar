use k8s_openapi::api::apps::v1::ReplicaSet;
use polar::cassini::CassiniClient;
use polar::graph::controller::{
    GraphController, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property,
};
use polar::graph::nodes::kube::KubeNodeKey;
use ractor::ActorProcessingErr;
use tracing::debug;

use crate::supervisor::{EmitDecision, ProjectionCache};

use super::{
    GraphOperable, handle_owner_refs, k8s_time_ms, now_ms, project_deletion_state,
    state_signature, ts,
};

impl GraphOperable for ReplicaSet {
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

        let replicas = status.replicas;
        let ready = status.ready_replicas.unwrap_or(0);
        let available = status.available_replicas.unwrap_or(0);

        // ReplicaSet conditions are sparse in practice; observation time is
        // the usual valid_from here, upgraded to the attested condition time
        // when one exists.
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

        let rs_key = KubeNodeKey::ReplicaSet {
            uid: uid.clone(),
            cluster_uid: cluster_uid.to_string(),
        };

        // ---- Anchor node ----

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: rs_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name)),
                ts("observed_at", now_ms()),
            ],
        }))?;

        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
            from: rs_key.clone().into_key(),
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
            handle_owner_refs(&owners, rs_key.clone(), graph, cluster_uid)?;
        }

        // ---- Immutable ReplicaSetState ----
        //
        // Written exclusively through UpdateState. The previous version cast
        // UpdateState AND a manual TRANSITIONED_TO EnsureEdge — if the
        // controller creates that edge itself (it does, per the schema),
        // the manual edge was at best redundant and at worst attached a
        // duplicate with divergent props.

        let state_key = KubeNodeKey::ReplicaSetState {
            uid: uid.clone(),
            valid_from: valid_from_ms.to_string(),
            cluster_uid: cluster_uid.to_string(),
        };

        let meaningful_props = vec![
            Property("replicas".into(), GraphValue::I64(replicas as i64)),
            Property("ready_replicas".into(), GraphValue::I64(ready as i64)),
            Property(
                "available_replicas".into(),
                GraphValue::I64(available as i64),
            ),
        ];
        let signature = state_signature(&meaningful_props);

        match cache.should_emit(
            "ReplicaSet".to_string(),
            cluster_uid.to_string(),
            uid.clone(),
            signature,
            valid_from_ms.to_string(),
        ) {
            EmitDecision::Suppress => {
                debug!("ReplicaSet {uid} state unchanged since last observation, suppressing write");
            }
            EmitDecision::Emit { .. } => {
                let mut props = meaningful_props;
                props.push(ts("valid_from", valid_from_ms));
                props.push(ts("observed_at", now_ms()));

                graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                    resource_key: rs_key.into_key(),
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

        // Previously wrote fabricated zero replica counts via a manual
        // UpsertNode + EnsureEdge path (bypassing HAS_STATE maintenance) and
        // no "Deleted" phase. Deletion attests one fact only.
        project_deletion_state(
            graph,
            KubeNodeKey::ReplicaSet {
                uid: uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            },
            KubeNodeKey::ReplicaSetState {
                uid,
                valid_from: now.to_string(),
                cluster_uid: cluster_uid.to_string(),
            },
            now,
        )
    }
}
