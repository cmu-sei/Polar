//! The git scheduler's supervisor, on the agent builder: health check, router, the Cassini client
//! and the subscription come from `AgentBuilder`, the graph controller from the shared helper.
//!
//! A discovered repository is looked up in the graph (a read through the controller), given a
//! default configuration if nothing is stored, and published for the observer. The drain is the
//! one every graph-using agent has: a window, the controller stopped, then the handover.

use crate::SERVICE_NAME;
use crate::config::{default_config, fetch_repo_observation_config};
use cassini_types::ClientEvent;
use git_agent_common::{ConfigurationEvent, GIT_REPO_CONFIG_EVENTS, publish_event};
use polar::GitRepositoryDiscoveredEvent;
use polar::cassini::TcpClient;
use polar::graph::controller::{GraphControllerMsg, GraphSignal};
use polar::graph::nodes::git::RepoId;
use polar::topics::GIT_REPOSITORY_DISCOVERED;
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

pub enum GitSchedulerMessage {
    ClientEvent {
        event: ClientEvent,
    },
    /// A repository some agent found, decoded.
    Discovered(GitRepositoryDiscoveredEvent),
    GraphSignal(GraphSignal),
    /// The health check ended the pod's life; the infrastructure waits for
    /// `ProceedWithShutdown`.
    PrepareShutdown,
    /// The drain window has elapsed.
    DrainElapsed,
}

/// What was published on the discovery topic. Anything that does not decode is logged and dropped.
pub fn decode(payload: &[u8]) -> Option<GitRepositoryDiscoveredEvent> {
    match rkyv::from_bytes::<GitRepositoryDiscoveredEvent, rkyv::rancor::Error>(payload) {
        Ok(event) => Some(event),
        Err(e) => {
            warn!("Failed to deserialize a repository discovery event: {e}");
            None
        }
    }
}

pub fn subscriptions(supervisor: &ActorRef<GitSchedulerMessage>) -> Vec<Subscription> {
    let supervisor = supervisor.clone();
    vec![Subscription::new(
        GIT_REPOSITORY_DISCOVERED,
        move |incoming| {
            if let Some(event) = decode(&incoming.payload)
                && let Err(e) = supervisor.cast(GitSchedulerMessage::Discovered(event))
            {
                error!("failed to hand a discovery to the supervisor: {e}");
            }
        },
    )]
}

pub struct GitSchedulerSupervisor;

pub struct GitSchedulerState {
    tcp_client: TcpClient,
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    graph_controller: Option<ActorRef<GraphControllerMsg>>,
    drain: Drain,
}

impl GitSchedulerSupervisor {
    /// Decides how the discovered repository is observed and publishes that for the observer.
    async fn schedule(
        event: GitRepositoryDiscoveredEvent,
        state: &GitSchedulerState,
    ) -> Result<(), ActorProcessingErr> {
        // A discovery with no HTTP URL used to panic here. It cannot be observed over HTTP, so it
        // is left alone.
        let Some(repo_url) = event.http_url else {
            warn!("a discovered repository has no HTTP URL, so it is not scheduled");
            return Ok(());
        };
        let Some(controller) = &state.graph_controller else {
            return Err("the graph controller is gone".into());
        };
        let repo_id = RepoId::from_url(&repo_url);
        let config = match fetch_repo_observation_config(controller, &repo_id).await? {
            Some(config) => {
                debug!("Fetched stored configuration for repo {repo_id}");
                config
            }
            None => {
                debug!("No stored configuration for repo {repo_id}, using defaults");
                default_config(repo_id, repo_url)
            }
        };
        publish_event(
            &state.tcp_client,
            GIT_REPO_CONFIG_EVENTS,
            &ConfigurationEvent { config },
        )?;
        Ok(())
    }
}

#[async_trait]
impl Actor for GitSchedulerSupervisor {
    type Msg = GitSchedulerMessage;
    type State = GitSchedulerState;
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let mut builder = AgentBuilder::new(myself.clone(), SERVICE_NAME)
            .with_lifecycle_handler(|event| Some(GitSchedulerMessage::ClientEvent { event }));
        for subscription in subscriptions(&myself) {
            builder = builder.with_subscription(subscription);
        }
        let resources = builder.into_configured(configured).build().await?;

        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => Some(GitSchedulerMessage::PrepareShutdown),
            });

        let graph_controller = spawn_graph_controller(
            &myself,
            format!("{SERVICE_NAME}.graph_controller"),
            &resources.config,
            GitSchedulerMessage::GraphSignal,
        )
        .await?;

        Ok(GitSchedulerState {
            tcp_client: resources.tcp_client,
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
            GitSchedulerMessage::GraphSignal(signal) => match signal {
                GraphSignal::Available => {
                    let _ = state.healthcheck.graph_available();
                }
                GraphSignal::OpFailed(reason) => {
                    let _ = state.healthcheck.graph_op_failed(reason);
                }
            },
            GitSchedulerMessage::ClientEvent { event } => match event {
                // Fires on every registration, a reconnect included; the builder has already
                // subscribed to the topic, so it only reports health.
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
            GitSchedulerMessage::Discovered(event) => {
                if state.drain.is_draining() {
                    warn!("draining -- a discovery will be lost on exit");
                } else if let Err(e) = Self::schedule(event, state).await {
                    // One repository that cannot be scheduled is not a reason to stop scheduling.
                    error!("could not schedule a discovered repository: {e}");
                }
            }
            GitSchedulerMessage::PrepareShutdown => {
                if state.drain.begin(&myself, drain_window(), true, || {
                    GitSchedulerMessage::DrainElapsed
                }) {
                    info!("PrepareShutdown received (rejuvenation); entering the drain window");
                } else {
                    warn!(
                        "PrepareShutdown while already draining; the pending exit ends the process"
                    );
                }
            }
            GitSchedulerMessage::DrainElapsed => {
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
                    GitSchedulerMessage::DrainElapsed
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

    #[test]
    fn a_discovery_decodes_with_or_without_an_http_url() {
        for http_url in [Some("https://example.org/r.git".to_string()), None] {
            let event = GitRepositoryDiscoveredEvent {
                http_url: http_url.clone(),
                ssh_url: None,
            };
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&event).unwrap();
            assert_eq!(decode(&bytes).unwrap().http_url, http_url);
        }
    }

    #[test]
    fn bytes_that_are_not_a_discovery_are_dropped() {
        assert!(decode(b"not an event").is_none());
    }
}
