//! What the operator tells the rest of Polar: one [`AgentLifecycleEvent`] for
//! each fact it observes, published on `polar.agents.lifecycle`.
//!
//! Deciding which events a pass produced is pure ([`transitions`],
//! [`deployment_declared`], [`started`]): it compares the status the operator
//! remembered with the one it is about to write. Publishing is best effort and never
//! stops a reconcile: the cluster, not the event stream, is the source of truth for
//! what is running, and the next declaration (sent once per deployment each time the
//! operator starts or reconnects) lets the graph catch up.
//!
//! This is the one place the operator says which target it is: every event names the
//! agent by the name its code declares, and the deployment and its instances as
//! `Kubernetes` variants.

use crate::crd::{Ending, PolarAgent, PolarAgentStatus};
use crate::pod;
use cassini_client::{OfflineBehavior, PublishRequest};
use kube::ResourceExt;
use polar::cassini::CassiniClient;
use polar::topics::AGENT_LIFECYCLE_EVENTS;
use polar::{AgentLifecycleEvent, DeploymentRef, InstanceRef, RkyvError};
use std::sync::Arc;
use tracing::{debug, warn};

/// Publishes lifecycle events, or drops them when the operator runs without a broker.
#[derive(Clone, Default)]
pub struct Publisher {
    client: Option<Arc<dyn CassiniClient>>,
}

impl Publisher {
    pub fn new(client: Arc<dyn CassiniClient>) -> Self {
        Self {
            client: Some(client),
        }
    }

    /// No broker: events are logged at debug and go nowhere.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn publish(&self, event: &AgentLifecycleEvent) {
        let Some(client) = &self.client else {
            debug!(?event, "no broker; lifecycle event not published");
            return;
        };
        let payload = match rkyv::to_bytes::<RkyvError>(event) {
            Ok(bytes) => bytes.to_vec(),
            Err(e) => {
                warn!("cannot serialize a lifecycle event: {e}");
                return;
            }
        };
        if let Err(e) = client.publish(PublishRequest {
            topic: AGENT_LIFECYCLE_EVENTS.to_string(),
            payload,
            trace_ctx: None,
            offline_behavior: OfflineBehavior::Queue,
        }) {
            warn!("lifecycle event not published: {e}");
        }
    }
}

/// The `PolarAgent` resource as a deployment.
pub fn deployment_ref(agent: &PolarAgent, cluster_uid: &str) -> DeploymentRef {
    DeploymentRef::Kubernetes {
        cluster_uid: cluster_uid.to_string(),
        namespace: agent.namespace().unwrap_or_default(),
        name: agent.name_any(),
        uid: agent.metadata.uid.clone().unwrap_or_default(),
    }
}

/// A pod of the deployment as an instance.
pub fn instance_ref(cluster_uid: &str, pod_uid: &str, pod_name: &str) -> InstanceRef {
    InstanceRef::KubernetesPod {
        cluster_uid: cluster_uid.to_string(),
        pod_uid: pod_uid.to_string(),
        pod_name: pod_name.to_string(),
    }
}

/// What the deployment declares.
pub fn deployment_declared(
    agent: &PolarAgent,
    deployment: DeploymentRef,
    hash: &str,
    now_ms: i64,
) -> AgentLifecycleEvent {
    AgentLifecycleEvent::DeploymentDeclared {
        agent: agent.agent_name(),
        deployment,
        spec_hash: hash.to_string(),
        image: agent.spec.image.clone(),
        shutdown_budget_secs: agent.spec.shutdown_budget_secs,
        identity: pod::service_account(agent),
        observed_at: now_ms,
    }
}

/// An instance the operator started (or, on a declaration, one it found running).
pub fn started(
    agent: &PolarAgent,
    deployment: DeploymentRef,
    instance: InstanceRef,
    number: u64,
    hash: &str,
    started_at: i64,
) -> AgentLifecycleEvent {
    AgentLifecycleEvent::InstanceStarted {
        agent: agent.agent_name(),
        deployment,
        instance,
        number,
        spec_hash: hash.to_string(),
        started_at,
    }
}

/// The facts a pass produced: an instance's ending, then the phase it led to.
pub fn transitions(
    agent: &str,
    deployment: &DeploymentRef,
    before: &PolarAgentStatus,
    after: &PolarAgentStatus,
    now_ms: i64,
) -> Vec<AgentLifecycleEvent> {
    let mut events = Vec::new();
    if after.last_ending != before.last_ending
        && let Some(ending) = &after.last_ending
    {
        events.push(ended(agent, deployment, ending, now_ms));
    }
    if after.phase != before.phase {
        events.push(AgentLifecycleEvent::DeploymentPhaseChanged {
            agent: agent.to_string(),
            deployment: deployment.clone(),
            phase: format!("{:?}", after.phase),
            message: after.message.clone(),
            observed_at: now_ms,
        });
    }
    events
}

fn ended(
    agent: &str,
    deployment: &DeploymentRef,
    ending: &Ending,
    now_ms: i64,
) -> AgentLifecycleEvent {
    let finished_at = chrono::DateTime::parse_from_rfc3339(&ending.finished_at)
        .map(|t| t.timestamp_millis())
        .unwrap_or(now_ms);
    let DeploymentRef::Kubernetes { cluster_uid, .. } = deployment;
    AgentLifecycleEvent::InstanceEnded {
        agent: agent.to_string(),
        deployment: deployment.clone(),
        instance: ending
            .pod_uid
            .as_deref()
            .map(|uid| instance_ref(cluster_uid, uid, &ending.pod)),
        kind: format!("{:?}", ending.kind),
        exit_code: ending.exit_code,
        reason: ending.reason.clone(),
        message: ending.message.clone(),
        ran_secs: ending.ran_secs,
        finished_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{EndingKind, Phase, PolarAgentSpec};
    use polar::cassini::{MockCassiniClient, RecordedCall};

    fn deployment() -> DeploymentRef {
        DeploymentRef::Kubernetes {
            cluster_uid: "cluster".into(),
            namespace: "polar-demo".into(),
            name: "steady".into(),
            uid: "cr-uid".into(),
        }
    }

    fn ending() -> Ending {
        Ending {
            pod: "steady-1".into(),
            pod_uid: Some("uid-1".into()),
            kind: EndingKind::Exited,
            exit_code: 1,
            reason: "Error".into(),
            message: Some("rejuvenating".into()),
            ran_secs: 45,
            finished_at: "2026-10-18T12:00:00Z".into(),
        }
    }

    #[test]
    fn an_ending_comes_before_the_phase_it_led_to() {
        let before = PolarAgentStatus {
            phase: Phase::Running,
            ..Default::default()
        };
        let after = PolarAgentStatus {
            phase: Phase::Ending,
            last_ending: Some(ending()),
            ..Default::default()
        };
        let events = transitions("steady", &deployment(), &before, &after, 0);
        assert!(matches!(
            &events[0],
            AgentLifecycleEvent::InstanceEnded {
                instance: Some(InstanceRef::KubernetesPod { pod_uid, .. }),
                kind,
                finished_at,
                ..
            } if pod_uid == "uid-1" && kind == "Exited" && *finished_at == 1_792_324_800_000
        ));
        assert!(matches!(
            &events[1],
            AgentLifecycleEvent::DeploymentPhaseChanged { phase, .. } if phase == "Ending"
        ));
    }

    #[test]
    fn nothing_changed_is_nothing_to_say() {
        let status = PolarAgentStatus {
            phase: Phase::Running,
            last_ending: Some(ending()),
            ..Default::default()
        };
        assert!(transitions("steady", &deployment(), &status, &status, 0).is_empty());
    }

    #[test]
    fn the_agent_is_named_by_its_code_not_by_its_deployment() {
        let mut agent = PolarAgent::new(
            "steady-east",
            PolarAgentSpec {
                agent: Some("steady".into()),
                image: "busybox:1.37".into(),
                command: vec![],
                args: vec![],
                env: vec![],
                needs: vec![],
                shutdown_budget_secs: 5,
                service_account: None,
                suspend: false,
            },
        );
        agent.metadata.namespace = Some("polar-demo".into());
        let event = deployment_declared(&agent, deployment_ref(&agent, "cluster"), "h", 0);
        assert_eq!(event.agent(), "steady");
        assert!(matches!(
            event,
            AgentLifecycleEvent::DeploymentDeclared {
                deployment: DeploymentRef::Kubernetes { name, .. },
                identity,
                ..
            } if name == "steady-east" && identity == "steady-east"
        ));
    }

    #[test]
    fn events_go_out_as_rkyv_on_the_lifecycle_topic() {
        let mock = Arc::new(MockCassiniClient::new());
        let publisher = Publisher::new(mock.clone());
        let event = AgentLifecycleEvent::DeploymentPhaseChanged {
            agent: "steady".into(),
            deployment: deployment(),
            phase: "Running".into(),
            message: None,
            observed_at: 7,
        };
        publisher.publish(&event);

        let calls = mock.calls();
        let [RecordedCall::Publish { topic, payload }] = calls.as_slice() else {
            panic!("expected one publish, got {calls:?}");
        };
        assert_eq!(topic, AGENT_LIFECYCLE_EVENTS);
        let decoded = rkyv::from_bytes::<AgentLifecycleEvent, RkyvError>(payload).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn without_a_broker_publishing_is_a_no_op() {
        Publisher::none().publish(&AgentLifecycleEvent::DeploymentPhaseChanged {
            agent: "steady".into(),
            deployment: deployment(),
            phase: "Running".into(),
            message: None,
            observed_at: 7,
        });
    }
}
