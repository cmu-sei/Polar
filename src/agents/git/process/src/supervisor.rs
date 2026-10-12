//! The git processor's supervisor, on the agent builder: health check, router, the Cassini client
//! and the subscription come from `AgentBuilder`, the graph controller from the shared helper.
//!
//! Each event the observer publishes is turned into graph operations (`ops`) and handed to the
//! controller. A failure of the controller starts a drain window and then ends the process, and the
//! Job starts a fresh one. Shutdown follows the same order as every graph-writing agent: the drain
//! window, the controller stopped waiting for a write in flight, then `ProceedWithShutdown`.

use crate::SERVICE_NAME;
use crate::ops;
use cassini_types::ClientEvent;
use git_agent_common::GitRepositoryMessage;
use polar::graph::controller::{GraphControllerMsg, GraphSignal};
use polar::topics::GIT_REPOSITORY_EVENTS;
use polar_agent_builder::{
    AgentBuilder, Configured, Drain, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage, Subscription, spawn_graph_controller, stop_graph_controller,
};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use tracing::{debug, error, info, warn};

const DRAIN_WINDOW_SECS: u64 = 5;
const GRAPH_STOP_SECS: u64 = 30;

/// How long the infrastructure supervisor gives this agent to drain: the drain window and the
/// graph controller's stop, plus a margin. Past it the process exits 1 regardless.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(DRAIN_WINDOW_SECS + GRAPH_STOP_SECS + 10);

fn drain_window() -> Duration {
    Duration::from_secs(DRAIN_WINDOW_SECS)
}

pub enum GitProcessorMessage {
    ClientEvent {
        event: ClientEvent,
    },
    /// An event the observer published, decoded.
    Event(GitRepositoryMessage),
    GraphSignal(GraphSignal),
    /// The health check ended the pod's life; the infrastructure waits for
    /// `ProceedWithShutdown`.
    PrepareShutdown,
    /// The drain window has elapsed: after a failure this exits, after `PrepareShutdown` it
    /// flushes the graph controller and completes the handshake.
    DrainElapsed,
}

/// What was published on the events topic. Anything that does not decode is logged and dropped.
pub fn decode(payload: &[u8]) -> Option<GitRepositoryMessage> {
    match rkyv::from_bytes::<GitRepositoryMessage, rkyv::rancor::Error>(payload) {
        Ok(event) => Some(event),
        Err(e) => {
            warn!("Failed to deserialize a git repository event: {e}");
            None
        }
    }
}

pub fn subscriptions(supervisor: &ActorRef<GitProcessorMessage>) -> Vec<Subscription> {
    let supervisor = supervisor.clone();
    vec![Subscription::new(GIT_REPOSITORY_EVENTS, move |incoming| {
        if let Some(event) = decode(&incoming.payload)
            && let Err(e) = supervisor.cast(GitProcessorMessage::Event(event))
        {
            error!("failed to hand an event to the supervisor: {e}");
        }
    })]
}

pub struct GitProcessorSupervisor;

pub struct GitProcessorState {
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    graph_controller: Option<ActorRef<GraphControllerMsg>>,
    drain: Drain,
}

#[async_trait]
impl Actor for GitProcessorSupervisor {
    type Msg = GitProcessorMessage;
    type State = GitProcessorState;
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let mut builder = AgentBuilder::new(myself.clone(), SERVICE_NAME)
            .with_lifecycle_handler(|event| Some(GitProcessorMessage::ClientEvent { event }));
        for subscription in subscriptions(&myself) {
            builder = builder.with_subscription(subscription);
        }
        let resources = builder.into_configured(configured).build().await?;

        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => Some(GitProcessorMessage::PrepareShutdown),
            });

        let graph_controller = spawn_graph_controller(
            &myself,
            format!("{SERVICE_NAME}.graph_controller"),
            &resources.config,
            GitProcessorMessage::GraphSignal,
        )
        .await?;

        Ok(GitProcessorState {
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            graph_controller: Some(graph_controller),
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
            GitProcessorMessage::GraphSignal(signal) => match signal {
                GraphSignal::Available => {
                    let _ = state.healthcheck.graph_available();
                }
                GraphSignal::OpFailed(reason) => {
                    let _ = state.healthcheck.graph_op_failed(reason);
                }
            },
            GitProcessorMessage::ClientEvent { event } => match event {
                // Fires on every registration, a reconnect included, and the builder has already
                // subscribed to the topic, so it only reports health. The graph controller is
                // started once, in pre_start: starting it here made the second registration fail
                // on the name the first had taken.
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
            GitProcessorMessage::Event(event) => {
                if state.drain.is_draining() {
                    warn!("draining -- an event will be lost on exit");
                } else if let Some(controller) = &state.graph_controller {
                    for op in ops::for_event(event) {
                        controller.cast(GraphControllerMsg::Op(op))?;
                    }
                }
            }
            GitProcessorMessage::PrepareShutdown => {
                if state.drain.begin(&myself, drain_window(), true, || {
                    GitProcessorMessage::DrainElapsed
                }) {
                    info!("PrepareShutdown received (rejuvenation); entering the drain window");
                } else {
                    warn!(
                        "PrepareShutdown while already draining; the pending exit ends the process"
                    );
                }
            }
            GitProcessorMessage::DrainElapsed => {
                warn!("drain window elapsed, beginning shutdown");
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
                    GitProcessorMessage::DrainElapsed
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
    use git_agent_common::GitRepositoryMessage;
    use polar::graph::nodes::git::RepoId;

    #[test]
    fn an_event_the_observer_published_decodes() {
        let event = GitRepositoryMessage::RefUpdated {
            repo: RepoId::new("r".into()),
            ref_name: "refs/heads/main".into(),
            old: None,
            new: "b".into(),
            observed_at: "1".into(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&event).unwrap();
        assert!(matches!(
            decode(&bytes),
            Some(GitRepositoryMessage::RefUpdated { .. })
        ));
    }

    #[test]
    fn bytes_that_are_not_an_event_are_dropped() {
        assert!(decode(b"not an event").is_none());
    }
}
