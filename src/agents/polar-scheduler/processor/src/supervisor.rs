//! Supervisor for the scheduler processor, on the agent builder: health check, router, the Cassini
//! client and the subscriptions come from `AgentBuilder`.
//!
//! The processor receives schedule changes (from the observer) and ad-hoc agent announcements,
//! records them in the graph and notifies the agents they concern. Children: the graph controller
//! and the processor, both linked, so a failure of either starts a drain window and then ends the
//! process, and the Job starts a fresh one.
//!
//! Shutdown, in order, when the health check ends the pod's life:
//!
//! 1. `InfrastructureEvent::PrepareShutdown` reaches this supervisor, which starts the drain window.
//! 2. When the window ends the processor and the graph controller are stopped, the latter waiting
//!    for any write in flight.
//! 3. `ProceedWithShutdown` goes to the infrastructure supervisor, which stops its children.
//! 4. The infrastructure supervisor's termination makes this supervisor exit non-zero, so the
//!    workload is restarted with fresh certificates.

use crate::processor::ScheduleInfoProcessor;
use crate::types::ProcessorMsg;
use cassini_types::ClientEvent;
use polar::graph::controller::{GraphControllerMsg, GraphSignal};
use polar_agent_builder::{
    AgentBuilder, Configured, Drain, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage, Subscription, spawn_graph_controller, stop_graph_controller,
};
use polar_scheduler_common::{
    AdhocAgentAnnouncement, CONFIGURATION_OUTCOME_TOPIC, ConfigurationOutcome, GitScheduleChange,
    SCHEDULER_ADHOC_TOPIC, SCHEDULER_IN_TOPIC,
};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use tracing::{debug, error, info, warn};

pub const SERVICE_NAME: &str = "polar.scheduler";

const DRAIN_WINDOW_SECS: u64 = 5;
const GRAPH_STOP_SECS: u64 = 30;

/// How long the infrastructure supervisor gives this agent to drain: the drain window and the
/// graph controller's stop, plus a margin. Past it the process exits 1 regardless.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(DRAIN_WINDOW_SECS + GRAPH_STOP_SECS + 10);

fn drain_window() -> Duration {
    Duration::from_secs(DRAIN_WINDOW_SECS)
}

pub enum SchedulerSupervisorMessage {
    ClientEvent {
        event: ClientEvent,
    },
    /// A schedule changed in the repository.
    ScheduleChanged(GitScheduleChange),
    /// An ad-hoc agent announced itself with a default schedule.
    Announced(AdhocAgentAnnouncement),
    /// An agent says what it did with a signed configuration document.
    Outcome {
        outcome: ConfigurationOutcome,
        reported_by: Option<String>,
    },
    GraphSignal(GraphSignal),
    /// The health check ended the pod's life; the infrastructure waits for
    /// `ProceedWithShutdown`.
    PrepareShutdown,
    /// The drain window has elapsed: after a failure this exits, after `PrepareShutdown` it
    /// flushes the graph controller and completes the handshake.
    DrainElapsed,
}

/// The processor's two subscriptions. Each payload is decoded where it arrives, so the supervisor
/// only sees values; one that does not decode is logged and dropped.
pub fn subscriptions(supervisor: &ActorRef<SchedulerSupervisorMessage>) -> Vec<Subscription> {
    let for_changes = supervisor.clone();
    let for_announcements = supervisor.clone();
    let for_outcomes = supervisor.clone();
    vec![
        Subscription::new(SCHEDULER_IN_TOPIC, move |incoming| {
            match rkyv::from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&incoming.payload) {
                Ok(change) => {
                    if let Err(e) =
                        for_changes.cast(SchedulerSupervisorMessage::ScheduleChanged(change))
                    {
                        error!("failed to hand a schedule change to the supervisor: {e}");
                    }
                }
                Err(e) => error!("failed to deserialize a GitScheduleChange: {e:?}"),
            }
        }),
        Subscription::new(
            SCHEDULER_ADHOC_TOPIC,
            move |incoming| match rkyv::from_bytes::<AdhocAgentAnnouncement, rkyv::rancor::Error>(
                &incoming.payload,
            ) {
                Ok(announcement) => {
                    if let Err(e) =
                        for_announcements.cast(SchedulerSupervisorMessage::Announced(announcement))
                    {
                        error!("failed to hand an announcement to the supervisor: {e}");
                    }
                }
                Err(e) => error!("failed to deserialize an ad-hoc announcement: {e:?}"),
            },
        ),
        Subscription::new(
            CONFIGURATION_OUTCOME_TOPIC,
            move |incoming| match rkyv::from_bytes::<ConfigurationOutcome, rkyv::rancor::Error>(
                &incoming.payload,
            ) {
                Ok(outcome) => {
                    if let Err(e) = for_outcomes.cast(SchedulerSupervisorMessage::Outcome {
                        outcome,
                        reported_by: incoming.published_by,
                    }) {
                        error!("failed to hand a configuration outcome to the supervisor: {e}");
                    }
                }
                Err(e) => error!("failed to deserialize a configuration outcome: {e:?}"),
            },
        ),
    ]
}

pub struct SchedulerSupervisor;

pub struct SchedulerSupervisorState {
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    graph_controller: Option<ActorRef<GraphControllerMsg>>,
    processor: Option<ActorRef<ProcessorMsg>>,
    drain: Drain,
}

#[async_trait]
impl Actor for SchedulerSupervisor {
    type Msg = SchedulerSupervisorMessage;
    type State = SchedulerSupervisorState;
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let mut builder =
            AgentBuilder::new(myself.clone(), SERVICE_NAME).with_lifecycle_handler(|event| {
                Some(SchedulerSupervisorMessage::ClientEvent { event })
            });
        for subscription in subscriptions(&myself) {
            builder = builder.with_subscription(subscription);
        }
        let resources = builder.into_configured(configured).build().await?;

        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => {
                    Some(SchedulerSupervisorMessage::PrepareShutdown)
                }
            });

        let graph_controller = spawn_graph_controller(
            &myself,
            format!("{SERVICE_NAME}.graph_controller"),
            &resources.config,
            SchedulerSupervisorMessage::GraphSignal,
        )
        .await?;

        let tcp_client = resources.tcp_client.as_actor_ref().clone();
        let (processor, _) = Actor::spawn_linked(
            Some(format!("{SERVICE_NAME}.processor")),
            ScheduleInfoProcessor,
            (tcp_client, graph_controller.clone()),
            myself.get_cell(),
        )
        .await?;

        Ok(SchedulerSupervisorState {
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            graph_controller: Some(graph_controller),
            processor: Some(processor),
            drain: Drain::default(),
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SchedulerSupervisorMessage::GraphSignal(signal) => match signal {
                GraphSignal::Available => {
                    let _ = state.healthcheck.graph_available();
                }
                GraphSignal::OpFailed(reason) => {
                    let _ = state.healthcheck.graph_op_failed(reason);
                }
            },
            SchedulerSupervisorMessage::ClientEvent { event } => match event {
                // Fires on every registration, a reconnect included, and the builder has already
                // subscribed to the topics, so it only reports health.
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cassini_connected();
                    let _ = state.healthcheck.graph_connected();
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => {}
            },
            SchedulerSupervisorMessage::ScheduleChanged(change) => {
                if state.drain.is_draining() {
                    warn!("draining -- a schedule change will be lost on exit");
                } else if let Some(processor) = &state.processor {
                    processor.cast(ProcessorMsg::GitChange(change))?;
                }
            }
            SchedulerSupervisorMessage::Announced(announcement) => {
                if state.drain.is_draining() {
                    warn!("draining -- an ad-hoc announcement will be lost on exit");
                } else if let Some(processor) = &state.processor {
                    processor.cast(ProcessorMsg::Announcement(announcement))?;
                }
            }
            SchedulerSupervisorMessage::Outcome {
                outcome,
                reported_by,
            } => {
                if state.drain.is_draining() {
                    warn!("draining -- a configuration outcome will be lost on exit");
                } else if let Some(processor) = &state.processor {
                    processor.cast(ProcessorMsg::Outcome {
                        outcome,
                        reported_by,
                    })?;
                }
            }
            SchedulerSupervisorMessage::PrepareShutdown => {
                if state.drain.begin(&myself, drain_window(), true, || {
                    SchedulerSupervisorMessage::DrainElapsed
                }) {
                    info!("PrepareShutdown received (rejuvenation); entering the drain window");
                } else {
                    warn!(
                        "PrepareShutdown while already draining; the pending exit ends the process"
                    );
                }
            }
            SchedulerSupervisorMessage::DrainElapsed => {
                warn!("drain window elapsed, beginning shutdown");
                if let Some(processor) = state.processor.take() {
                    processor.stop(Some("supervisor shutting down".into()));
                }
                if let Some(controller) = state.graph_controller.take() {
                    stop_graph_controller(controller, Duration::from_secs(GRAPH_STOP_SECS)).await;
                }
                state.drain.complete(&state.infrastructure);
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorTerminated(actor, _, reason) => {
                // The infrastructure supervisor ending is the end of the handshake, or
                // infrastructure dying on its own; either way the process exits so the Job
                // starts a pod with fresh certificates.
                if actor.get_id() == state.infrastructure.get_id() {
                    info!("infrastructure terminated ({reason:?}), exiting");
                    std::process::exit(1);
                }
                warn!("Actor {:?} terminated: {reason:?}", actor.get_name());
            }
            SupervisionEvent::ActorFailed(actor, err) => {
                error!("Actor {:?} failed: {err:?}", actor.get_name());
                if state.drain.begin(&myself, drain_window(), false, || {
                    SchedulerSupervisorMessage::DrainElapsed
                }) {
                    warn!("entering drain window before exit");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_router::IncomingMessage;
    use polar_scheduler_common::{AdhocAgentAnnouncement, GitScheduleChange};
    use std::sync::{Arc, Mutex};
    use std::time::Duration as StdDuration;

    /// Collects what the subscriptions hand to the supervisor.
    struct Sink {
        got: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl Actor for Sink {
        type Msg = SchedulerSupervisorMessage;
        type State = Arc<Mutex<Vec<&'static str>>>;
        type Arguments = ();
        async fn pre_start(
            &self,
            _: ActorRef<Self::Msg>,
            _: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(self.got.clone())
        }
        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            message: Self::Msg,
            state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            let kind = match message {
                SchedulerSupervisorMessage::ScheduleChanged(_) => "change",
                SchedulerSupervisorMessage::Announced(_) => "announcement",
                SchedulerSupervisorMessage::Outcome { reported_by, .. } => {
                    if reported_by.as_deref() == Some("the-broker-says") {
                        "outcome from the broker's identity"
                    } else {
                        "outcome"
                    }
                }
                _ => "other",
            };
            state.lock().unwrap().push(kind);
            Ok(())
        }
    }

    fn message(topic: &str, payload: Vec<u8>) -> IncomingMessage {
        IncomingMessage {
            topic: topic.into(),
            payload,
            published_by: None,
            trace_ctx: None,
        }
    }

    #[tokio::test]
    async fn each_topic_reaches_the_supervisor_as_its_own_message_and_garbage_is_dropped() {
        let got = Arc::new(Mutex::new(Vec::new()));
        let (sink, _) = Actor::spawn(None, Sink { got: got.clone() }, ())
            .await
            .unwrap();
        let subs = subscriptions(&sink);
        let topics: Vec<&str> = subs.iter().map(|s| s.topic.as_str()).collect();
        assert_eq!(
            topics,
            [
                SCHEDULER_IN_TOPIC,
                SCHEDULER_ADHOC_TOPIC,
                CONFIGURATION_OUTCOME_TOPIC
            ]
        );

        let change = rkyv::to_bytes::<rkyv::rancor::Error>(&GitScheduleChange::Delete {
            path: "a.dhall".into(),
        })
        .unwrap()
        .to_vec();
        let announcement = rkyv::to_bytes::<rkyv::rancor::Error>(&AdhocAgentAnnouncement {
            agent_type: "worker".into(),
            version: 1,
            default_schedule_json: "{}".into(),
        })
        .unwrap()
        .to_vec();

        (subs[0].deliver)(message(SCHEDULER_IN_TOPIC, change));
        (subs[1].deliver)(message(SCHEDULER_ADHOC_TOPIC, announcement));
        (subs[0].deliver)(message(SCHEDULER_IN_TOPIC, vec![1, 2, 3]));
        (subs[1].deliver)(message(SCHEDULER_ADHOC_TOPIC, vec![9]));

        // An outcome keeps the identity the broker gave for whoever published it.
        let outcome = rkyv::to_bytes::<rkyv::rancor::Error>(&ConfigurationOutcome {
            agent: "a".into(),
            id: None,
            version: None,
            status: "applied".into(),
            changed: vec![],
            detail: vec![],
            signer: None,
            at_unix_secs: 1,
            sequence: 1,
        })
        .unwrap()
        .to_vec();
        let mut from_broker = message(CONFIGURATION_OUTCOME_TOPIC, outcome);
        from_broker.published_by = Some("the-broker-says".into());
        (subs[2].deliver)(from_broker);
        (subs[2].deliver)(message(CONFIGURATION_OUTCOME_TOPIC, vec![7, 7]));

        tokio::time::sleep(StdDuration::from_millis(100)).await;
        assert_eq!(
            *got.lock().unwrap(),
            [
                "change",
                "announcement",
                "outcome from the broker's identity"
            ]
        );
    }
}
