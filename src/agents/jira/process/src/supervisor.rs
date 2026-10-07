use crate::BROKER_CLIENT_NAME;
use crate::{groups, issues, projects, users};
use cassini_types::ClientEvent;
use jira_common::types::JiraData;
use jira_common::{
    JIRA_GROUPS_CONSUMER_TOPIC, JIRA_ISSUES_CONSUMER_TOPIC, JIRA_PROJECTS_CONSUMER_TOPIC,
    JIRA_USERS_CONSUMER_TOPIC,
};
use polar::cassini::{CassiniClient, TcpClient};
use polar::graph::controller::{
    GraphController, GraphControllerActor, GraphControllerArgs, GraphSignal,
};
use polar_agent_builder::{
    AgentBuilder, HealthClient, HealthHandle, InfrastructureEvent, InfrastructureSupervisorMessage,
    Subscription,
};
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use ractor::async_trait;
use ractor::concurrency::Duration;
use ractor::{Actor, OutputPort};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Every topic this agent consumes. One Subscription each; all of them
/// deliver to the supervisor, which decodes and writes to the graph.
const JIRA_TOPICS: [&str; 4] = [
    JIRA_PROJECTS_CONSUMER_TOPIC,
    JIRA_GROUPS_CONSUMER_TOPIC,
    JIRA_USERS_CONSUMER_TOPIC,
    JIRA_ISSUES_CONSUMER_TOPIC,
];

/// How long the infrastructure supervisor gives this agent to drain: the graph
/// controller's stop (up to 30 s) plus a margin. If it is not done by then the
/// process exits 1 regardless.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(30 + 10);

/// This supervisor's own message type. polar::SupervisorMessage cannot
/// carry the two infrastructure signals without polar-agent-builder
/// depending back on lib, so the builder is generic over the message and
/// each supervisor defines what it needs (see kube-observer).
pub enum ConsumerSupervisorMessage {
    ClientEvent {
        event: ClientEvent,
    },
    GraphSignal(GraphSignal),
    /// InfrastructureSupervisor replaced the TcpClient after an unexpected
    /// termination. The builder re-fires every Subscription when the new
    /// client registers; this supervisor only needs the new handle.
    InfrastructureRespawned {
        tcp_client: TcpClient,
    },
    /// The HealthCheckActor, a child of InfrastructureSupervisor, has
    /// terminated for rejuvenation. Finish this supervisor's own work,
    /// then send InfrastructureSupervisorMessage::ProceedWithShutdown.
    PrepareShutdown,
}

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    broker_client: TcpClient,
    graph_controller: Option<GraphController>,
    healthcheck: HealthHandle,
    /// Sent ProceedWithShutdown once the graph controller has flushed.
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    /// True once PrepareShutdown has been received. Messages that still
    /// arrive are logged, not written, and a linked actor stopping is
    /// expected rather than fatal.
    shutting_down: bool,
}

/// Decodes what an observer published: the payload of the event is the
/// serialized JiraData. Anything that does not decode is logged and
/// dropped, never fatal.
pub fn decode_payload(payload: &[u8]) -> Option<JiraData> {
    match rkyv::from_bytes::<JiraData, rkyv::rancor::Error>(payload) {
        Ok(data) => Some(data),
        Err(e) => {
            warn!("Failed to deserialize jira data: {e:?}");
            None
        }
    }
}

impl ConsumerSupervisor {
    fn dispatch(
        topic: &str,
        payload: &[u8],
        graph_controller: &GraphController,
    ) -> Result<(), ActorProcessingErr> {
        let Some(data) = decode_payload(payload) else {
            warn!("Dropping undecodable message on topic '{topic}'");
            return Ok(());
        };
        match data {
            JiraData::Projects(items) => projects::handle_projects(&items, graph_controller),
            JiraData::Groups(items) => groups::handle_groups(&items, graph_controller),
            JiraData::Users(items) => users::handle_users(&items, graph_controller),
            JiraData::Issues(issue) => issues::handle_issue(&issue.json, graph_controller),
            #[allow(unreachable_patterns)]
            _ => {
                warn!("Ignoring unexpected jira data on topic '{topic}'");
                Ok(())
            }
        }
    }

    /// One Subscription per topic. Each deliver closure rebuilds the
    /// ClientEvent::MessagePublished the supervisor's handle() expects and
    /// forwards it. AgentBuilder fires the wire-level Subscribe for every
    /// one of them when the client registers, and again after a reconnect.
    fn jira_subscriptions(supervisor: &ActorRef<ConsumerSupervisorMessage>) -> Vec<Subscription> {
        JIRA_TOPICS
            .iter()
            .map(|topic| {
                let supervisor = supervisor.clone();
                Subscription::new(*topic, move |incoming| {
                    let event = ClientEvent::MessagePublished {
                        topic: incoming.topic,
                        payload: incoming.payload,
                        trace_ctx: incoming.trace_ctx,
                        published_by: incoming.published_by,
                    };
                    if let Err(e) =
                        supervisor.send_message(ConsumerSupervisorMessage::ClientEvent { event })
                    {
                        warn!("AgentBuilder: failed to deliver routed message to supervisor: {e}");
                    }
                })
            })
            .collect()
    }
}

#[async_trait]
impl Actor for ConsumerSupervisor {
    type Msg = ConsumerSupervisorMessage;
    type State = ConsumerSupervisorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        // HealthCheckActor, RouterActor, the four topic subscriptions and
        // the TcpClient in one composed call. The builder checks that the
        // deployment provides graph-write (POLAR_CAPABILITIES) before
        // spawning anything, and reads the dependency endpoints from
        // POLAR_HEALTH_DEP_ENDPOINTS. This agent never encrypts a payload,
        // so it does not declare encryption.
        let mut builder = AgentBuilder::new(myself.clone(), BROKER_CLIENT_NAME)
            .with_shutdown_budget(SHUTDOWN_BUDGET)
            .with_graph_write();
        for subscription in Self::jira_subscriptions(&myself) {
            builder = builder.with_subscription(subscription);
        }
        let resources = builder
            .with_lifecycle_handler(|event| Some(ConsumerSupervisorMessage::ClientEvent { event }))
            .build()
            .await?;

        // The infrastructure half of the respawn and shutdown protocol.
        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::Respawned { tcp_client, .. } => {
                    Some(ConsumerSupervisorMessage::InfrastructureRespawned { tcp_client })
                }
                InfrastructureEvent::PrepareShutdown => {
                    Some(ConsumerSupervisorMessage::PrepareShutdown)
                }
            });

        // Eager graph controller spawn: its health never needs to wait on
        // Cassini registration. GraphControllerActor::pre_start only builds
        // pool config; HealthCheckMessage::GraphAvailable is what proves
        // real connectivity.
        let graph_signal_port = Arc::new(OutputPort::<GraphSignal>::default());
        graph_signal_port.subscribe(myself.clone(), |signal| {
            Some(ConsumerSupervisorMessage::GraphSignal(signal))
        });
        let (graph_controller, _) = GraphControllerActor::spawn_linked(
            Some("jira.process.graph.controller".to_string()),
            GraphControllerActor,
            GraphControllerArgs {
                signal_port: graph_signal_port,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        Ok(ConsumerSupervisorState {
            broker_client: resources.tcp_client,
            graph_controller: Some(graph_controller),
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            shutting_down: false,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ConsumerSupervisorMessage::GraphSignal(signal) => match signal {
                GraphSignal::Available => {
                    let _ = state.healthcheck.graph_available();
                }
                GraphSignal::OpFailed(reason) => {
                    let _ = state.healthcheck.graph_op_failed(reason);
                }
            },
            ConsumerSupervisorMessage::InfrastructureRespawned { tcp_client } => {
                warn!("CONSUMER_SUPERVISOR: TCP client was respawned; subscriptions are restored on its next registration");
                state.broker_client = tcp_client;
            }
            ConsumerSupervisorMessage::PrepareShutdown => {
                info!(
                    "CONSUMER_SUPERVISOR: PrepareShutdown received -- unsubscribing and \
                     flushing the graph controller before infrastructure goes down"
                );
                state.shutting_down = true;

                for topic in JIRA_TOPICS {
                    if let Err(e) =
                        state
                            .broker_client
                            .unsubscribe(polar::cassini::UnsubscribeRequest {
                                topic: topic.to_string(),
                                trace_ctx: None,
                            })
                    {
                        warn!(
                            "CONSUMER_SUPERVISOR: unsubscribe from '{topic}' failed (continuing): {e}"
                        );
                    }
                }

                if let Some(controller) = state.graph_controller.take() {
                    match controller
                        .stop_and_wait(
                            Some("supervisor shutting down".into()),
                            Some(Duration::from_secs(30)),
                        )
                        .await
                    {
                        Ok(()) => {
                            info!("CONSUMER_SUPERVISOR: graph controller stopped cleanly");
                        }
                        Err(e) => {
                            error!(
                                "CONSUMER_SUPERVISOR: graph controller did not stop cleanly \
                                 within 30s, continuing: {e:?}"
                            );
                        }
                    }
                }

                let _ = state
                    .infrastructure
                    .cast(InfrastructureSupervisorMessage::ProceedWithShutdown);
            }
            ConsumerSupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    // AgentBuilder already fired the wire-level Subscribe
                    // for every topic when this event fired.
                    let _ = state.healthcheck.cassini_connected();
                    let _ = state.healthcheck.graph_connected();
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    if state.shutting_down {
                        warn!(
                            "CONSUMER_SUPERVISOR: shutting down -- logging message on topic \
                             '{topic}' ({} bytes) instead of writing; will be lost on exit",
                            payload.len()
                        );
                        // TODO: publish to dead-letter queue once Cassini supports one.
                    } else if let Some(controller) = &state.graph_controller {
                        Self::dispatch(&topic, &payload, controller)?;
                    } else {
                        return Err(ActorProcessingErr::from(
                            "a message arrived but there is no graph controller",
                        ));
                    }
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => (),
            },
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(actor_cell) => {
                info!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} started",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                info!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                // InfrastructureSupervisor is this supervisor's linked child
                // from build(); the HealthCheckActor is its child, not this
                // supervisor's. Its termination means the shutdown handshake
                // is complete, or that infrastructure itself failed. Either
                // way, exit non-zero so the Job creates a new pod with fresh
                // certs. Matched by id: it is spawned unnamed.
                if actor_cell.get_id() == state.infrastructure.get_id() {
                    info!("CONSUMER_SUPERVISOR: infrastructure is down, exiting");
                    std::process::exit(1);
                }
                // Any other linked actor (the graph controller) stopping
                // outside a shutdown means writes can no longer happen.
                if !state.shutting_down {
                    error!(
                        "CONSUMER_SUPERVISOR: a linked actor terminated unexpectedly, \
                         graph writes can no longer happen; exiting"
                    );
                    std::process::exit(1);
                }
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                error!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                // Any linked actor failing is fatal: exit so the Job
                // controller creates a new pod.
                std::process::exit(1);
            }
            SupervisionEvent::ProcessGroupChanged(..) => {}
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jira_common::types::JiraProject;

    /// What an observer puts on the wire for this agent: the serialized
    /// JiraData, nothing else.
    fn as_published(data: &JiraData) -> Vec<u8> {
        rkyv::to_bytes::<rkyv::rancor::Error>(data)
            .unwrap()
            .into_vec()
    }

    #[test]
    fn decodes_what_an_observer_publishes() {
        let data = JiraData::Projects(vec![JiraProject {
            id: "10042".to_string(),
            key: "PLAT".to_string(),
            name: "Platform".to_string(),
            project_type_key: "software".to_string(),
        }]);

        let decoded = decode_payload(&as_published(&data));

        assert!(matches!(
            decoded,
            Some(JiraData::Projects(ref p)) if p.len() == 1 && p[0].id == "10042"
        ));
    }

    #[test]
    fn a_payload_that_does_not_decode_is_dropped_not_fatal() {
        assert!(decode_payload(&[0u8; 3]).is_none());
    }

    #[test]
    fn every_consumed_topic_is_subscribed_exactly_once() {
        let mut topics = JIRA_TOPICS.to_vec();
        topics.sort();
        topics.dedup();
        assert_eq!(topics.len(), 4);
    }
}
