use k8s_openapi::api::batch::v1::Job;
use polar::cassini::CassiniClient;
use polar::graph::controller::{
    GraphController, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property,
};
use polar::graph::nodes::kube::KubeNodeKey;
use ractor::ActorProcessingErr;
use tracing::debug;

use crate::supervisor::{EmitDecision, ProjectionCache};

use super::{
    GraphOperable, handle_owner_refs, k8s_time_ms, now_ms, project_deletion_state, state_signature,
    ts,
};

impl GraphOperable for Job {
    fn project_into_graph(
        self,
        graph: &GraphController,
        _tcp_client: &dyn CassiniClient,
        cluster_uid: &str,
        cache: &mut ProjectionCache,
    ) -> Result<(), ActorProcessingErr> {
        // A resource without a uid has no identity to project. Defaulting to
        // "" would merge every uid-less resource into one node keyed on the
        // empty string — silently corrupting, so skip instead.
        // TODO: emit a tracing::warn! here once logging is wired into this
        // crate; a uid-less watch event is worth knowing about.
        let Some(uid) = self.metadata.uid.clone() else {
            return Ok(());
        };
        let name = self.metadata.name.clone().unwrap_or_default();
        let namespace = self
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());

        // Surface the cyclops.build/id label so the Cyclops graph processor
        // can write EXECUTED_IN edges by uid without knowing it came from k8s.
        // Other labels are not individually surfaced — they're noise at this level.
        let cyclops_build_id = self
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("cyclops.build/id"))
            .cloned()
            .unwrap_or_default();

        let job_key = KubeNodeKey::Job {
            uid: uid.clone(),
            cluster_uid: cluster_uid.to_string(),
        };

        // ---- Anchor node ----

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: job_key.clone().into_key(),
            props: vec![
                Property("name".into(), GraphValue::String(name.clone())),
                Property(
                    "cyclops_build_id".into(),
                    GraphValue::String(cyclops_build_id),
                ),
                ts("observed_at", now_ms()),
            ],
        }))?;

        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
            from: job_key.clone().into_key(),
            rel_type: "IN_NAMESPACE".into(),
            to: KubeNodeKey::Namespace {
                name: namespace.clone(),
                cluster_uid: cluster_uid.to_string(),
            }
            .into_key(),
            props: Vec::new(),
        }))?;

        // ---- Owner refs (e.g. CronJob owns Job) ----

        if let Some(owners) = self.metadata.owner_references {
            handle_owner_refs(&owners, job_key.clone(), graph, cluster_uid)?;
        }

        // ---- State ----

        let status = self.status.as_ref();

        let active = status.and_then(|s| s.active).unwrap_or(0);
        let succeeded = status.and_then(|s| s.succeeded).unwrap_or(0);
        let failed = status.and_then(|s| s.failed).unwrap_or(0);

        // Derive a human-readable phase from the same conditions the
        // orchestrator's interpret_job_status uses, for consistency.
        let phase = if succeeded > 0 {
            "Succeeded"
        } else if failed > 0 && active == 0 {
            "Failed"
        } else if active > 0 {
            "Running"
        } else {
            "Pending"
        };

        let failure_reason = status
            .and_then(|s| s.conditions.as_ref())
            .and_then(|conds| conds.iter().find(|c| c.type_ == "Failed"))
            .and_then(|c| c.message.clone())
            .unwrap_or_default();

        // completion_time is k8s-attested; a job still running has none, so
        // fall back to observation time for the interim state instance. The
        // terminal (completed) observation will carry the attested time.
        let valid_from_ms = status
            .and_then(|s| s.completion_time.as_ref())
            .map(k8s_time_ms)
            .unwrap_or_else(now_ms);

        let state_key = KubeNodeKey::JobState {
            uid: uid.clone(),
            // State keys embed the same epoch-ms value as valid_from so the
            // key remains deterministic and collision-free per transition.
            valid_from: valid_from_ms.to_string(),
            cluster_uid: cluster_uid.to_string(),
        };

        let meaningful_props = vec![
            Property("phase".into(), GraphValue::String(phase.into())),
            Property("active".into(), GraphValue::I64(active as i64)),
            Property("succeeded".into(), GraphValue::I64(succeeded as i64)),
            Property("failed".into(), GraphValue::I64(failed as i64)),
            Property("failure_reason".into(), GraphValue::String(failure_reason)),
        ];
        let signature = state_signature(&meaningful_props);

        match cache.should_emit(
            "Job".to_string(),
            cluster_uid.to_string(),
            uid.clone(),
            signature,
            valid_from_ms.to_string(),
        ) {
            EmitDecision::Suppress => {
                debug!("Job {uid} state unchanged since last observation, suppressing write");
            }
            EmitDecision::Emit => {
                let mut props = meaningful_props;
                props.push(ts("valid_from", valid_from_ms));
                props.push(ts("observed_at", now_ms()));

                graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                    resource_key: job_key.clone().into_key(),
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

        project_deletion_state(
            graph,
            KubeNodeKey::Job {
                uid: uid.clone(),
                cluster_uid: cluster_uid.to_string(),
            },
            KubeNodeKey::JobState {
                uid,
                valid_from: now.to_string(),
                cluster_uid: cluster_uid.to_string(),
            },
            now,
        )
    }
}
