use crate::{
    CredentialConfigError, GitRepoSupervisor, GitRepoSupervisorArgs, HostCredentialConfig,
    RepoSupervisorMessage, StaticCredentialConfig, REPO_SUPERVISOR_NAME, SERVICE_NAME,
};
use cassini_client::TcpClientMessage;
use cassini_types::ClientEvent;
use git_agent_common::{ConfigurationEvent, GIT_REPO_CONFIG_EVENTS};
use polar::SupervisorMessage;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent};
use rkyv::from_bytes;
use std::path::PathBuf;
use tracing::{debug, error, info, instrument, warn};

pub struct RootSupervisor;

pub struct RootSupervisorState {
    repo_supervisor: Option<ActorRef<RepoSupervisorMessage>>,
    tcp_client: ActorRef<TcpClientMessage>,
}

impl RootSupervisor {
    pub fn deserialize_and_dispatch(
        topic: String,
        payload: Vec<u8>,
        state: &mut RootSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Received message from topic {topic}");
        if let Ok(ev) = from_bytes::<ConfigurationEvent, rkyv::rancor::Error>(&payload) {
            if let Some(s) = &state.repo_supervisor {
                return Ok(s.cast(RepoSupervisorMessage::SpawnWorker { config: ev.config })?);
            } else {
                return Err("Failed to find repo supervisor".into());
            }
        } else {
            warn!("Failed to deserialize event");
            return Ok(());
        }
    }

    #[instrument(skip_all, level = "debug")]
    async fn init(
        myself: ActorRef<SupervisorMessage>,
        state: &mut RootSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        // try to read config file
        match StaticCredentialConfig::from_env("GIT_AGENT_CONFIG") {
            Ok(credential_config) => {
                let cache_root = match std::env::var("POLAR_CACHE_ROOT") {
                    Ok(path) => {
                        debug!("Using cache root at {}", path);
                        PathBuf::from(path)
                    }
                    Err(_) => {
                        let default_dir = ".polar/cache";

                        debug!(
                            "Attempting to create default cache directory {}",
                            default_dir
                        );
                        if let Ok(current_dir) = std::env::current_dir() {
                            current_dir.join(default_dir)
                        } else {
                            return Err(ActorProcessingErr::from("Failed to determine cache root"));
                        }
                    }
                };

                // start repo supervior
                //
                let (repo_supervisor, _) = Actor::spawn_linked(
                    Some(REPO_SUPERVISOR_NAME.to_string()),
                    GitRepoSupervisor,
                    GitRepoSupervisorArgs {
                        credential_config,
                        tcp_client: state.tcp_client.clone(),
                        cache_root: cache_root.clone(),
                    },
                    myself.into(),
                )
                .await?;

                state.repo_supervisor = Some(repo_supervisor);

                // subscribe to configuration messages
                state.tcp_client.cast(TcpClientMessage::Subscribe(
                    GIT_REPO_CONFIG_EVENTS.to_string(),
                ))?;

                Ok(())
            }
            Err(e) => match e {
                CredentialConfigError::Io(e) => Err(e.into()),
                CredentialConfigError::Parse(e) => Err(e.into()),
            },
        }
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
        // Read Kubernetes credentials and other data from the environment
        debug!("{myself:?} starting");

        let tcp_client = polar::spawn_tcp_client(SERVICE_NAME, myself, |event| {
            Some(SupervisorMessage::ClientEvent { event })
        })
        .await?;

        Ok(RootSupervisorState {
            tcp_client,
            repo_supervisor: None,
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
                ClientEvent::Registered { .. } => Self::init(myself, state).await?,
                ClientEvent::MessagePublished { topic, payload } => {
                    Self::deserialize_and_dispatch(topic, payload, state)?
                }
                ClientEvent::TransportError { reason } => {
                    error!("Transport error occurred! {reason}");
                    myself.stop(Some(reason))
                }
            },
        }
        Ok(())
    }
}
