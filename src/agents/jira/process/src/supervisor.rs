use crate::BROKER_CLIENT_NAME;
use crate::{groups, issues, projects, users};
use cassini_types::{ClientEvent, ClientMessage};
use jira_common::types::JiraData;
use jira_common::{
    JIRA_GROUPS_CONSUMER_TOPIC, JIRA_ISSUES_CONSUMER_TOPIC, JIRA_PROJECTS_CONSUMER_TOPIC,
    JIRA_USERS_CONSUMER_TOPIC,
};
use polar::SupervisorMessage;
use polar::cassini::{CassiniClient, TcpClient};
use polar::graph::controller::{
    GraphController, GraphControllerActor, GraphControllerArgs, GraphSignal,
};
use polar_agent_builder::{AgentBuilder, HealthClient, HealthHandle, Subscription};
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use ractor::async_trait;
use ractor::concurrency::Duration;
use ractor::{Actor, OutputPort};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

const DRAIN_WINDOW_SECS: u64 = 5;

/// Every topic this agent consumes. One Subscription each; all of them
/// deliver to the supervisor, which decodes and writes to the graph.
const JIRA_TOPICS: [&str; 4] = [
    JIRA_PROJECTS_CONSUMER_TOPIC,
    JIRA_GROUPS_CONSUMER_TOPIC,
    JIRA_USERS_CONSUMER_TOPIC,
    JIRA_ISSUES_CONSUMER_TOPIC,
];

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    broker_client: TcpClient,
    graph_controller: Option<GraphController>,
    healthcheck: HealthHandle,
    /// Set true once a fatal failure or rejuvenation has started the
    /// bounded drain window. While true, incoming messages are logged
    /// instead of written, since we are exiting shortly regardless.
    draining: bool,
}

/// Unwraps what an observer published: the payload of the event is a
/// serialized ClientMessage::PublishRequest whose own payload is the
/// serialized JiraData. Anything that does not decode is logged and
/// dropped, never fatal.
pub fn decode_payload(payload: &[u8]) -> Option<JiraData> {
    let message = match rkyv::from_bytes::<ClientMessage, rkyv::rancor::Error>(payload) {
        Ok(message) => message,
        Err(e) => {
            warn!("Failed to deserialize message: {e:?}");
            return None;
        }
    };
    match message {
        ClientMessage::PublishRequest { payload, .. } => {
            match rkyv::from_bytes::<JiraData, rkyv::rancor::Error>(&payload) {
                Ok(data) => Some(data),
                Err(e) => {
                    warn!("Failed to deserialize jira data: {e:?}");
                    None
                }
            }
        }
        other => {
            warn!("Received unexpected ClientMessage variant: {other:?}");
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
    /// forwards it as a SupervisorMessage::ClientEvent. AgentBuilder fires
    /// the wire-level Subscribe for every one of them when the client
    /// registers, and again after a reconnect.
    fn jira_subscriptions(supervisor: &ActorRef<SupervisorMessage>) -> Vec<Subscription> {
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
                    if let Err(e) = supervisor.send_message(SupervisorMessage::ClientEvent { event })
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
    type Msg = SupervisorMessage;
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
        let mut builder = AgentBuilder::new(myself.clone(), BROKER_CLIENT_NAME).with_graph_write();
        for subscription in Self::jira_subscriptions(&myself) {
            builder = builder.with_subscription(subscription);
        }
        let resources = builder
            .with_lifecycle_handler(|event| Some(SupervisorMessage::ClientEvent { event }))
            .build()
            .await?;

        // Eager graph controller spawn: its health never needs to wait on
        // Cassini registration. GraphControllerActor::pre_start only builds
        // pool config; HealthCheckMessage::GraphAvailable is what proves
        // real connectivity.
        let graph_signal_port = Arc::new(OutputPort::<GraphSignal>::default());
        graph_signal_port.subscribe(myself.clone(), |signal| {
            Some(SupervisorMessage::GraphSignal(signal))
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
            draining: false,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::Heartbeat => {}
            SupervisorMessage::GraphSignal(signal) => match signal {
                GraphSignal::Available => {
                    let _ = state.healthcheck.graph_available();
                }
                GraphSignal::OpFailed(reason) => {
                    let _ = state.healthcheck.graph_op_failed(reason);
                }
            },
            SupervisorMessage::ForceExit => {
                warn!("CONSUMER_SUPERVISOR: drain window elapsed, beginning graceful shutdown");

                if let Some(controller) = state.graph_controller.take() {
                    warn!(
                        "CONSUMER_SUPERVISOR: stopping graph controller and waiting for any \
                         in-flight write to complete before exiting"
                    );
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
                                 within 30s, exiting anyway: {e:?}"
                            );
                        }
                    }
                }

                warn!("CONSUMER_SUPERVISOR: exiting now");
                std::process::exit(1);
            }
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    // AgentBuilder already fired the wire-level Subscribe
                    // for every topic when this event fired.
                    let _ = state.healthcheck.cassini_connected();
                    let _ = state.healthcheck.graph_connected();
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    if state.draining {
                        warn!(
                            "CONSUMER_SUPERVISOR: draining -- logging message on topic \
                             '{topic}' ({} bytes) instead of writing; will be lost on exit",
                            payload.len()
                        );
                        // TODO: publish to dead-letter queue once Cassini supports one.
                    } else if let Some(controller) = &state.graph_controller {
                        Self::dispatch(&topic, &payload, controller)?;
                    } else {
                        error!("No graph controller present!");
                        myself.stop(None);
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
        myself: ActorRef<Self::Msg>,
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
                // The healthcheck actor terminating cleanly means the
                // rejuvenation sequence completed; any other linked actor
                // terminating (the graph controller) means writes can no
                // longer happen. Both end in the same bounded drain and a
                // non-zero exit so the Job restarts the pod.
                //
                // The healthcheck actor is matched by id: AgentBuilder
                // spawns it unnamed.
                if !state.draining {
                    state.draining = true;
                    if actor_cell.get_id() == state.healthcheck.as_actor_ref().get_id() {
                        info!(
                            "CONSUMER_SUPERVISOR: healthcheck actor terminated cleanly, \
                             entering drain window before exiting for rejuvenation, \
                             unsubscribing from Cassini"
                        );
                        for topic in JIRA_TOPICS {
                            if let Err(e) =
                                state.broker_client.unsubscribe(polar::cassini::UnsubscribeRequest {
                                    topic: topic.to_string(),
                                    trace_ctx: None,
                                })
                            {
                                warn!(
                                    "CONSUMER_SUPERVISOR: unsubscribe from '{topic}' failed \
                                     (continuing): {e}"
                                );
                            }
                        }
                    } else {
                        warn!(
                            "CONSUMER_SUPERVISOR: a linked actor terminated, so graph writes \
                             can no longer happen; entering drain window before exit"
                        );
                    }
                    let _ = myself
                        .send_after(Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                error!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                // Any linked actor failing is fatal, but not immediately:
                // give the mailbox a bounded window to keep logging
                // in-flight messages before ForceExit actually exits.
                if !state.draining {
                    state.draining = true;
                    warn!(
                        "CONSUMER_SUPERVISOR: entering drain window before exit; \
                         any messages that arrive will be logged, not written"
                    );
                    let _ = myself
                        .send_after(Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
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

    /// What an observer puts on the wire for this agent: a serialized
    /// PublishRequest wrapping the serialized JiraData.
    fn as_published(data: &JiraData) -> Vec<u8> {
        let inner = rkyv::to_bytes::<rkyv::rancor::Error>(data).unwrap();
        let message = ClientMessage::PublishRequest {
            topic: JIRA_PROJECTS_CONSUMER_TOPIC.to_string(),
            payload: inner.to_vec().into(),
            registration_id: "test".to_string(),
            trace_ctx: None,
            share_identity: true,
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&message)
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
