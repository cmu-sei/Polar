use crate::{
    CredentialLookup, GitAgentConfig, GitRepoSupervisor, GitRepoSupervisorArgs,
    REPO_SUPERVISOR_NAME, RepoObservationConfig, RepoSupervisorMessage, SERVICE_NAME,
};
use cassini_types::ClientEvent;
use polar::{
    GitRepositoryDiscoveredEvent, SupervisorMessage,
    cassini::{CassiniClient, SubscribeRequest, TcpClient},
    graph::nodes::git::RepoId,
    topics::GIT_REPOSITORY_DISCOVERED,
};
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use rkyv::from_bytes;
use std::path::PathBuf;
use tracing::{debug, error, instrument, warn};

pub struct RootSupervisor;

pub struct RootSupervisorState {
    repo_supervisor: Option<ActorRef<RepoSupervisorMessage>>,
    tcp_client: TcpClient,
    git_agent_config: GitAgentConfig,
}

impl RootSupervisor {
    pub fn deserialize_and_dispatch(
        topic: String,
        payload: Vec<u8>,
        state: &mut RootSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Received message from topic {topic}");

        let ev = match from_bytes::<GitRepositoryDiscoveredEvent, rkyv::rancor::Error>(&payload) {
            Ok(ev) => ev,
            Err(_) => {
                warn!("Failed to deserialize discovery event on topic {topic}");
                return Ok(());
            }
        };

        let repo_url = match (ev.http_url, ev.ssh_url) {
            (Some(http), _) => http,
            (None, Some(_)) => {
                // Only the HTTP path is supported. The one current producer of
                // discovery events is hardcoded to send http_url, so this branch
                // should be unreachable in practice. If you're hitting this,
                // SSH support needs to be implemented for real.
                todo!("SSH-only repo discovery is not currently supported")
            }
            (None, None) => {
                warn!(
                    "Discovery event on topic {topic} had neither http_url nor ssh_url, dropping"
                );
                return Ok(());
            }
        };

        let normalized = RepoId::normalize_repo_url(&repo_url);
        let repo_id = RepoId::from_url(&normalized);

        let credentials = match state.git_agent_config.credentials_for_url(&normalized) {
            CredentialLookup::Configured(c) => Some(c),
            CredentialLookup::NotConfigured => None,
            CredentialLookup::Misconfigured => {
                error!(
                    "repo {normalized} has a credentials entry with no usable token — check {}",
                    "POLAR_GIT_AGENT_CONFIG"
                );
                None
            }
        };

        let config = match credentials {
            Some(creds) => RepoObservationConfig::new_with_credentials(
                repo_id,
                repo_url.clone(),
                vec!["origin".to_string()],
                None,
                vec![],
                creds,
            ),
            None => RepoObservationConfig::new(
                repo_id,
                repo_url.clone(),
                vec!["origin".to_string()],
                None,
                vec![],
            ),
        };

        if let Some(s) = &state.repo_supervisor {
            Ok(s.cast(RepoSupervisorMessage::SpawnWorker { config })?)
        } else {
            Err("Failed to find repo supervisor".into())
        }
    }

    #[instrument(skip_all, level = "debug")]
    async fn init(
        myself: ActorRef<SupervisorMessage>,
        state: &mut RootSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        let cache_root = match std::env::var("POLAR_CACHE_ROOT") {
            Ok(path) => {
                debug!("Using cache root at {}", path);
                PathBuf::from(path)
            }
            Err(_) => {
                let default_dir = ".polar/cache";
                debug!(
                    "No POLAR_CACHE_ROOT set, using default cache directory {}",
                    default_dir
                );
                if let Ok(current_dir) = std::env::current_dir() {
                    current_dir.join(default_dir)
                } else {
                    return Err(ActorProcessingErr::from("Failed to determine cache root"));
                }
            }
        };

        let (repo_supervisor, _) = Actor::spawn_linked(
            Some(REPO_SUPERVISOR_NAME.to_string()),
            GitRepoSupervisor,
            GitRepoSupervisorArgs {
                tcp_client: state.tcp_client.clone(),
                cache_root,
            },
            myself.into(),
        )
        .await?;

        state.repo_supervisor = Some(repo_supervisor);

        // subscribe to incoming repo discovery events
        state.tcp_client.subscribe(SubscribeRequest {
            topic: GIT_REPOSITORY_DISCOVERED.to_string(),
            trace_ctx: None,
        })?;

        Ok(())
    }
}

#[async_trait]
impl Actor for RootSupervisor {
    type Msg = SupervisorMessage;
    type State = RootSupervisorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let config_path =
            std::env::var("POLAR_GIT_AGENT_CONFIG").unwrap_or_else(|_| "git.yaml".to_string());
        let git_agent_config = GitAgentConfig::load(std::path::Path::new(&config_path))
            .map_err(|e| ActorProcessingErr::from(format!("failed to load {config_path}: {e}")))?;

        let tcp_client = TcpClient::spawn(SERVICE_NAME, myself, |event| {
            Some(SupervisorMessage::ClientEvent { event })
        })
        .await?;

        Ok(RootSupervisorState {
            tcp_client,
            repo_supervisor: None,
            git_agent_config,
        })
    }

    async fn handle_supervisor_evt(
        &self,
        _: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                debug!(
                    "{0:?}:{1:?} terminated. {reason:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                error!(
                    "{0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    if let Err(e) = Self::init(myself.clone(), state).await {
                        myself.stop(Some(e.to_string()));
                    }
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    Self::deserialize_and_dispatch(topic, payload, state)?
                }
                ClientEvent::TransportError { reason } => {
                    error!("Transport error occurred! {reason}");
                    myself.stop(Some(reason))
                }
                ClientEvent::ControlResponse { .. } => {
                    error!("ControlResponse not implemented!");
                }
                _ => warn!("{event:?}"),
            },
        }
        Ok(())
    }
}
