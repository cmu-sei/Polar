use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent;
use git_agent_common::{
    GitRepositoryMessage, RepoObservationConfig, GIT_REPO_CONFIG_REQUESTS,
    GIT_REPO_CONFIG_RESPONSES,
};
use polar::SupervisorMessage;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent};
use rkyv::{from_bytes, rancor, to_bytes};
use tracing::{debug, error, info, instrument, trace, warn};

pub const SERVICE_NAME: &str = "polar.git.scheduler";
pub const TCP: &str = "tcp";

pub struct RootSupervisor;

pub struct RootSupervisorState {
    tcp_client: ActorRef<TcpClientMessage>,
}

impl RootSupervisor {
    #[instrument(skip_all, level = "debug")]
    async fn init(
        myself: ActorRef<SupervisorMessage>,
        state: &mut RootSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} initializing");
        // subscribe to supervision events
        state.tcp_client.cast(TcpClientMessage::Subscribe(
            GIT_REPO_CONFIG_REQUESTS.to_string(),
        ))?;

        Ok(())
    }

    pub fn deserialize_and_dispatch(
        topic: String,
        payload: Vec<u8>,
        state: &mut RootSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        trace!("Received message from topic {topic}");
        let message = from_bytes::<GitRepositoryMessage, rancor::Error>(&payload)?;

        match message {
            GitRepositoryMessage::ConfigurationRequest { repo_url } => {
                trace!("Received configuration request for repository {repo_url}");
                // Handle configuration request
                // Example: Fetch repository configuration and send response
                // query neo4j and send response
                // TODO: Implement fetching repository configuration from Neo4j, rem
                let response = GitRepositoryMessage::ConfigurationResponse {
                    config: RepoObservationConfig::new(
                        repo_url,
                        vec!["origin".to_string()],
                        Some(100),
                        vec!["refs/heads/main".to_string()],
                    ),
                };
                let payload = to_bytes::<rancor::Error>(&response)?;

                // Send response to the client
                let message = TcpClientMessage::Publish {
                    topic: GIT_REPO_CONFIG_RESPONSES.to_string(),
                    payload: payload.to_vec(),
                };

                state.tcp_client.cast(message)?;
            }
            _ => warn!("Received unknown message from TCP client {message:?}"),
        }

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
            Some(format!("{SERVICE_NAME}.{TCP}")),
            TcpClientActor,
            TcpClientArgs {
                config,
                registration_id: None,
                events_output,
            },
            myself.clone().into(),
        )
        .await?;
        Ok(RootSupervisorState { tcp_client })
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

#[tokio::main]
async fn main() {
    polar::init_logging(SERVICE_NAME.to_string());

    let (scheduler, handle) = Actor::spawn(
        Some(format!("{SERVICE_NAME}.supervisor")),
        RootSupervisor,
        (),
    )
    .await
    .unwrap();

    handle.await.unwrap();
}
