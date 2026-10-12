//! The git observer's supervisor: the builder's infrastructure (health, the Cassini client and the
//! subscription), the repository supervisor, and the order they come down in.
//!
//! The repository supervisor is started once, here, before the client has registered. It used to be
//! started when the client registered, and a registration that came again (after a dropped
//! connection) tried to start it a second time and ended the agent.

use crate::settings::ObserverSettings;
use crate::{
    GitAgentConfig, GitRepoSupervisor, GitRepoSupervisorArgs, REPO_SUPERVISOR_NAME,
    RepoSupervisorMessage,
};
use cassini_types::ClientEvent;
use git_agent_common::{ConfigurationEvent, GIT_REPO_CONFIG_EVENTS, RepoObservationConfig};
use polar::cassini::{CassiniClient, TcpClient, UnsubscribeRequest};
use polar_agent_builder::{
    AgentBuilder, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage, Subscription,
};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use tracing::{debug, error, info, warn};

/// What the observer needs to drain: the fetch in flight (up to 30 s; a worker fetches inside its
/// handler, so stopping it waits for the fetch) and a margin. If it is not done by then the
/// process exits 1 regardless.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(30 + 10);

/// How long the repository supervisor, and the workers under it, are given to stop.
const WORKERS_GRACE: Duration = Duration::from_secs(30);

pub enum RootSupervisorMessage {
    /// Lifecycle events from the Cassini client.
    ClientEvent { event: ClientEvent },
    /// A repository to observe, decoded from the configuration topic.
    Observe(Box<RepoObservationConfig>),
    /// The infrastructure has decided the agent should end: finish, then say so.
    PrepareShutdown,
}

pub struct RootSupervisor;

pub struct RootSupervisorState {
    tcp_client: TcpClient,
    repo_supervisor: Option<ActorRef<RepoSupervisorMessage>>,
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    shutting_down: bool,
}

impl RootSupervisor {
    /// What was published on the configuration topic. Anything that does not decode is logged and
    /// dropped, never fatal.
    pub fn decode(payload: &[u8]) -> Option<RepoObservationConfig> {
        match rkyv::from_bytes::<ConfigurationEvent, rkyv::rancor::Error>(payload) {
            Ok(event) => Some(event.config),
            Err(e) => {
                warn!("Failed to deserialize a repository configuration event: {e}");
                None
            }
        }
    }
}

#[async_trait]
impl Actor for RootSupervisor {
    type Msg = RootSupervisorMessage;
    type State = RootSupervisorState;
    /// The configuration `run` judged; the builder is made from it.
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        let settings = ObserverSettings::read(configured.config())?;

        // Startup validation: fail fast if the credential config is malformed. NOTE: not
        // currently threaded into the forwarding of a repository's configuration; whether per-repo
        // credential injection happens elsewhere is still to be settled with whoever wrote
        // GitAgentConfig.
        let config_path = settings.agent_config.clone();
        let _git_agent_config = GitAgentConfig::load(&config_path).map_err(|e| {
            ActorProcessingErr::from(format!("failed to load {}: {e}", config_path.display()))
        })?;
        let cache_root = settings
            .absolute_cache_root()
            .map_err(|_| ActorProcessingErr::from("Failed to determine cache root"))?;
        debug!("Using cache root at {}", cache_root.display());

        let deliver_to = myself.clone();
        let configuration = Subscription::new(GIT_REPO_CONFIG_EVENTS, move |incoming| {
            if let Some(config) = Self::decode(&incoming.payload)
                && let Err(e) =
                    deliver_to.send_message(RootSupervisorMessage::Observe(Box::new(config)))
            {
                warn!("failed to hand a repository to the supervisor: {e}");
            }
        });

        // The observer never touches the graph: Cassini only.
        let resources = AgentBuilder::new(myself.clone(), crate::agent::AGENT_NAME)
            .with_subscription(configuration)
            .with_lifecycle_handler(|event| Some(RootSupervisorMessage::ClientEvent { event }))
            .into_configured(configured)
            .build()
            .await?;

        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => Some(RootSupervisorMessage::PrepareShutdown),
            });

        let (repo_supervisor, _) = Actor::spawn_linked(
            Some(REPO_SUPERVISOR_NAME.to_string()),
            GitRepoSupervisor,
            GitRepoSupervisorArgs {
                tcp_client: resources.tcp_client.clone(),
                cache_root,
            },
            myself.get_cell(),
        )
        .await?;

        Ok(RootSupervisorState {
            tcp_client: resources.tcp_client,
            repo_supervisor: Some(repo_supervisor),
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
            RootSupervisorMessage::ClientEvent { event } => match event {
                // Registered fires on every registration, including a resume after a dropped
                // connection, so it does health and nothing else.
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cassini_connected();
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => (),
            },
            RootSupervisorMessage::Observe(config) => {
                if state.shutting_down {
                    warn!(
                        "shutting down -- not starting to observe {}",
                        config.repo_url
                    );
                } else if let Some(repo_supervisor) = &state.repo_supervisor {
                    repo_supervisor.cast(RepoSupervisorMessage::SpawnWorker { config: *config })?;
                } else {
                    return Err("Failed to find repo supervisor".into());
                }
            }
            RootSupervisorMessage::PrepareShutdown => {
                info!(
                    "ROOT_SUPERVISOR: PrepareShutdown received -- unsubscribing and stopping the workers"
                );
                state.shutting_down = true;
                if let Err(e) = state.tcp_client.unsubscribe(UnsubscribeRequest {
                    topic: GIT_REPO_CONFIG_EVENTS.to_string(),
                    trace_ctx: None,
                }) {
                    warn!("unsubscribe from '{GIT_REPO_CONFIG_EVENTS}' failed (continuing): {e}");
                }
                if let Some(repo_supervisor) = state.repo_supervisor.take()
                    && let Err(e) = repo_supervisor
                        .stop_and_wait(Some("supervisor shutting down".into()), Some(WORKERS_GRACE))
                        .await
                {
                    error!(
                        "the repository supervisor did not stop within {}s, continuing: {e:?}",
                        WORKERS_GRACE.as_secs()
                    );
                }
                let _ = state
                    .infrastructure
                    .cast(InfrastructureSupervisorMessage::ProceedWithShutdown);
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorStarted(actor) => debug!("{actor:?} started!"),
            SupervisionEvent::ActorTerminated(actor, _, reason) => {
                info!("ROOT_SUPERVISOR: {actor:?} terminated. {reason:?}");
                // The infrastructure is this supervisor's linked child from build(); its ending
                // means the shutdown handshake is done, or that it failed. Either way exit
                // non-zero so the Job creates a new pod with fresh certificates.
                if actor.get_id() == state.infrastructure.get_id() {
                    info!("ROOT_SUPERVISOR: infrastructure is down, exiting");
                    std::process::exit(1);
                }
                if !state.shutting_down {
                    error!("ROOT_SUPERVISOR: the repository supervisor ended unexpectedly; exiting");
                    std::process::exit(1);
                }
            }
            SupervisionEvent::ActorFailed(actor, reason) => {
                error!("ROOT_SUPERVISOR: {actor:?} failed! {reason:?}");
                if !state.shutting_down {
                    std::process::exit(1);
                }
            }
            _ => (),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar::graph::nodes::git::RepoId;

    #[test]
    fn a_payload_that_is_not_an_event_is_dropped_not_fatal() {
        assert!(RootSupervisor::decode(b"not an archived event").is_none());
        assert!(RootSupervisor::decode(&[]).is_none());
    }

    #[test]
    fn a_repository_event_decodes_to_the_repository_it_named() {
        let id = RepoId::from_url("https://example.org/org/repo.git");
        let config = RepoObservationConfig::new(
            id,
            "https://example.org/org/repo.git".into(),
            Vec::new(),
            Some(50),
            vec!["refs/heads/main".into()],
        );
        let bytes =
            rkyv::to_bytes::<rkyv::rancor::Error>(&ConfigurationEvent { config }).unwrap();
        let decoded = RootSupervisor::decode(&bytes).expect("it decodes");
        assert_eq!(decoded.repo_url, "https://example.org/org/repo.git");
        assert_eq!(decoded.refs, vec!["refs/heads/main".to_string()]);
    }
}
