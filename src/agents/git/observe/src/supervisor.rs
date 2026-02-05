use crate::{
    GitRepoSupervisor, GitRepoSupervisorArgs, RepoSupervisorMessage, REPO_SUPERVISOR_NAME,
    SERVICE_NAME,
};
use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent;
use git2::{Oid, Repository};
use git_agent_common::GIT_REPO_CONFIG_RESPONSES;
use polar::{Supervisor, SupervisorMessage};
use ractor::{
    async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort,
    SupervisionEvent,
};
use rkyv::{from_bytes, Archive, Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tokio::task::spawn_blocking;
use tracing::{debug, error, info, instrument, trace, warn};

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
        trace!("Received message from topic {topic}");
        let message = from_bytes::<RepoSupervisorMessage, rkyv::rancor::Error>(&payload)?;

        trace!("Forwarding message to repo supervisor");
        if let Some(repo_supervisor) = &state.repo_supervisor {
            Ok(repo_supervisor.cast(message)?)
        } else {
            return Err(ActorProcessingErr::from(
                "missing repo supervisor".to_string(),
            ));
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
                tcp_client: state.tcp_client.clone(),
                cache_root: cache_root.clone(),
            },
            myself.into(),
        )
        .await?;

        state.repo_supervisor = Some(repo_supervisor);

        // subscribe to configuration messages
        state.tcp_client.cast(TcpClientMessage::Subscribe(
            GIT_REPO_CONFIG_RESPONSES.to_string(),
        ))?;

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
        // Read Kubernetes credentials and other data from the environment
        debug!("{myself:?} starting");

        let events_output = std::sync::Arc::new(OutputPort::default());

        //subscribe to registration event
        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        let config = TCPClientConfig::new()?;

        let (tcp_client, _) = Actor::spawn_linked(
            Some(format!("{SERVICE_NAME}.supervisor.tcp")),
            TcpClientActor,
            TcpClientArgs {
                config,
                registration_id: None,
                events_output,
            },
            myself.clone().into(),
        )
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
                info!(
                    "CLUSTER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!(
                    "CLUSTER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
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
