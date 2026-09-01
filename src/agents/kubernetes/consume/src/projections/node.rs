use k8s_openapi::api::core::v1::Node;
use polar::cassini::CassiniClient;
use polar::graph::controller::{
    GraphController, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property,
};
use polar::graph::nodes::kube::KubeNodeKey;
use ractor::ActorProcessingErr;
use tracing::debug;

use crate::supervisor::{EmitDecision, ProjectionCache};

use super::{GraphOperable, now_ms, project_deletion_state, state_signature, ts};

// ---------------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------------
//
// Cluster-scoped, not namespaced -- no IN_NAMESPACE edge, no owner refs
// (nothing owns a Node). The one thing this impl does that no other impl in
// this crate needs to: record the name -> UID mapping in ProjectionCache,
// since Pod only ever carries a Node's *name* (spec.nodeName) and the k8s
// API exposes no way to learn a Node's UID from anywhere but the Node
// object itself. See ProjectionCache::record_node_uid/resolve_node_uid and
// Pod's RUNNING_ON edge.

impl GraphOperable for Node {
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

        let node_key = KubeNodeKey::Node {
            uid: uid.clone(),
            cluster_uid: cluster_uid.to_string(),
        };

        // ---- Anchor node ----
        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: node_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name.clone())),
                ts("observed_at", now_ms()),
            ],
        }))?;

        // Register this Node's identity for RUNNING_ON resolution *before*
        // any early return below -- a Node with no conditions reported yet
        // is still a Node a Pod can be scheduled onto, and Pod's edge
        // shouldn't have to wait on kubelet status reporting to resolve.
        cache.record_node_uid(cluster_uid.to_string(), name.clone(), uid.clone());

        // ---- State ----
        //
        // Ready/MemoryPressure/DiskPressure/PIDPressure/NetworkUnavailable
        // are the standard node conditions; unschedulable (cordon) lives on
        // spec, not status, but is exactly as state-like as any condition,
        // so it's tracked here rather than treated as a static anchor prop.
        //
        // valid_from uses last_transition_time, never last_heartbeat_time --
        // the kubelet updates last_heartbeat_time roughly every 10s
        // regardless of whether anything changed, which would make
        // valid_from differ on every single observation and defeat
        // ProjectionCache suppression entirely for this resource, the same
        // trap Deployment/ReplicaSet's condition handling already avoids.

        let Some(status) = self.status.as_ref() else {
            return Ok(());
        };
        let Some(conditions) = status.conditions.as_ref() else {
            return Ok(());
        };

        let cond_true = |kind: &str| {
            conditions
                .iter()
                .find(|c| c.type_ == kind)
                .map(|c| c.status == "True")
                .unwrap_or(false)
        };

        let ready = cond_true("Ready");
        let memory_pressure = cond_true("MemoryPressure");
        let disk_pressure = cond_true("DiskPressure");
        let pid_pressure = cond_true("PIDPressure");
        let network_unavailable = cond_true("NetworkUnavailable");

        let unschedulable = self
            .spec
            .as_ref()
            .and_then(|s| s.unschedulable)
            .unwrap_or(false);

        let valid_from_ms = conditions
            .iter()
            .filter_map(|c| c.last_transition_time.as_ref().map(|t| t.0.as_millisecond()))
            .max()
            .unwrap_or_else(now_ms);

        let state_key = KubeNodeKey::NodeState {
            uid: uid.clone(),
            valid_from: valid_from_ms.to_string(),
            cluster_uid: cluster_uid.to_string(),
        };

        let meaningful_props = vec![
            Property("ready".into(), GraphValue::Bool(ready)),
            Property("memory_pressure".into(), GraphValue::Bool(memory_pressure)),
            Property("disk_pressure".into(), GraphValue::Bool(disk_pressure)),
            Property("pid_pressure".into(), GraphValue::Bool(pid_pressure)),
            Property(
                "network_unavailable".into(),
                GraphValue::Bool(network_unavailable),
            ),
            Property("unschedulable".into(), GraphValue::Bool(unschedulable)),
        ];
        let signature = state_signature(&meaningful_props);

        match cache.should_emit(
            "Node".to_string(),
            cluster_uid.to_string(),
            uid.clone(),
            signature,
            valid_from_ms.to_string(),
        ) {
            EmitDecision::Suppress => {
                debug!("Node {uid} state unchanged since last observation, suppressing write");
            }
            EmitDecision::Emit => {
                let mut props = meaningful_props;
                props.push(ts("valid_from", valid_from_ms));
                props.push(ts("observed_at", now_ms()));

                graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                    resource_key: node_key.into_key(),
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
        cache: &mut ProjectionCache,
    ) -> Result<(), ActorProcessingErr> {
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let name = self.metadata.name.clone().unwrap_or_default();
        let now = now_ms();

        project_deletion_state(
            graph,
            KubeNodeKey::Node {
                uid: uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            },
            KubeNodeKey::NodeState {
                uid: uid.clone(),
                valid_from: now.to_string(),
                cluster_uid: cluster_uid.to_string(),
            },
            now,
        )?;

        cache.evict("Node".to_string(), cluster_uid.to_string(), &uid);

        // Closes the staleness window flagged when RUNNING_ON was first
        // built: without this, a deleted Node's name -> uid mapping stayed
        // resolvable until something else happened to overwrite it, so a
        // same-named replacement Node showing up before that overwrite
        // could have a Pod's RUNNING_ON resolve to the deleted Node's UID.
        cache.forget_node_uid(cluster_uid, &name);

        Ok(())
    }
}
