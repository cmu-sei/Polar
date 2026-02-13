use std::time::Duration;

use crate::GitlabConsumer;
use crate::GitlabConsumerState;
// use crate::groups::GitlabGroupConsumer;
// use crate::meta::MetaConsumer;
// use crate::pipelines::GitlabPipelineConsumer;
use crate::projects::GitlabProjectConsumer;
// use crate::repositories::GitlabRepositoryConsumer;
// use crate::runners::GitlabRunnerConsumer;
use crate::users::GitlabUserConsumer;
use crate::GitlabGraphController;
use crate::BROKER_CLIENT_NAME;
use cassini_backoff::{Backoff, ExponentialBackoff};
use cassini_client::*;
use cassini_types::ClientEvent;
use common::types::GitlabData;
use common::types::GitlabEnvelope;

use common::GROUPS_CONSUMER_TOPIC;
use common::METADATA_CONSUMER_TOPIC;
use common::PIPELINE_CONSUMER_TOPIC;
use common::PROJECTS_CONSUMER_TOPIC;
use common::REPOSITORY_CONSUMER_TOPIC;
use common::RUNNERS_CONSUMER_TOPIC;
use common::USER_CONSUMER_TOPIC;
use polar::get_neo_config;
use polar::Supervisor;
use polar::SupervisorMessage;
use ractor::async_trait;
use ractor::registry::where_is;
use ractor::rpc::call;
use ractor::rpc::CallResult;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::OutputPort;
use ractor::SupervisionEvent;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;
use tracing::{instrument, trace};

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    graph_config: neo4rs::Config,
    tcp_client: ActorRef<TcpClientMessage>,
    u_consumer: Option<GitlabConsumer>,
}

impl Supervisor for ConsumerSupervisor {
    #[instrument(level = "trace", fields(topic=topic))]
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>) {
        match rkyv::from_bytes::<GitlabEnvelope, rkyv::rancor::Error>(&payload) {
            Ok(message) => {
                if let Some(consumer) = ractor::registry::where_is(topic.clone()) {
                    trace!("Forwarding message to {topic}");
                    if let Err(e) = consumer.send_message(message) {
                        tracing::warn!("Error forwarding message. {e}");
                    }
                }
            }
            Err(err) => warn!("Failed to deserialize message: {:?}", err),
        }
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

        let events_output = std::sync::Arc::new(OutputPort::default());
        //subscribe
        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        let client_config = TCPClientConfig::new()?;

        let (tcp_client, _) = Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config: client_config,
                registration_id: None,
                events_output,
            },
            myself.clone().into(),
        )
        .await?;

        let graph_config = get_neo_config()?;
        let state = ConsumerSupervisorState {
            tcp_client,
            graph_config: graph_config,
            u_consumer: None,
        };

        Ok(state)
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
                    let graph = neo4rs::Graph::connect(state.graph_config.clone())?;

                    let (graph_controller, _) = Actor::spawn_linked(
                        None,
                        GitlabGraphController,
                        graph,
                        myself.clone().into(),
                    )
                    .await?;

                    // TODO: I guess technically we coul wrap this state in an arc and not have to copy it at all?
                    let c_state = GitlabConsumerState {
                        graph_controller,
                        tcp_client: state.tcp_client.clone(),
                    };

                    // if let Err(e) = Actor::spawn_linked(
                    //     Some(METADATA_CONSUMER_TOPIC.to_string()),
                    //     MetaConsumer,
                    //     graph_controller.clone(),
                    //     myself.clone().into(),
                    // )
                    // .await
                    // {
                    //     error!("failed to start meta consumer. {e}");
                    //     myself.stop(None);
                    // }
                    state.u_consumer = Some(
                        Actor::spawn_linked(
                            Some(USER_CONSUMER_TOPIC.to_string()),
                            GitlabUserConsumer,
                            c_state.clone(),
                            myself.clone().into(),
                        )
                        .await?
                        .0,
                    );

                    // if let Err(e) = Actor::spawn_linked(
                    //     Some(GROUPS_CONSUMER_TOPIC.to_string()),
                    //     GitlabGroupConsumer,
                    //     graph_controller.clone(),
                    //     myself.clone().into(),
                    // )
                    // .await
                    // {
                    //     error!("failed to start groups consumer. {e}");
                    //     myself.stop(None);
                    // }
                    // if let Err(e) = Actor::spawn_linked(
                    //     Some(RUNNERS_CONSUMER_TOPIC.to_string()),
                    //     GitlabRunnerConsumer,
                    //     graph_controller.clone(),
                    //     myself.clone().into(),
                    // )
                    // .await
                    // {
                    //     error!("failed to start runners consumer. {e}");
                    //     myself.stop(None);
                    // }
                    if let Err(e) = Actor::spawn_linked(
                        Some(PROJECTS_CONSUMER_TOPIC.to_string()),
                        GitlabProjectConsumer,
                        c_state.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start projects consumer. {e}");
                        myself.stop(None);
                    }
                    // if let Err(e) = Actor::spawn_linked(
                    //     Some(PIPELINE_CONSUMER_TOPIC.to_string()),
                    //     GitlabPipelineConsumer,
                    //     graph_controller.clone(),
                    //     myself.clone().into(),
                    // )
                    // .await
                    // {
                    //     error!("failed to start pipeline consumer. {e}");
                    //     myself.stop(None);
                    // }
                    // if let Err(e) = Actor::spawn_linked(
                    //     Some(REPOSITORY_CONSUMER_TOPIC.to_string()),
                    //     GitlabRepositoryConsumer,
                    //     graph_controller.clone(),
                    //     myself.clone().into(),
                    // )
                    // .await
                    // {
                    //     error!("failed to start pipeline consumer. {e}");
                    //     myself.stop(None);
                    // }
                }
                ClientEvent::MessagePublished { topic, payload } => {
                    ConsumerSupervisor::deserialize_and_dispatch(topic, payload);
                }
                ClientEvent::TransportError { .. } => todo!("Handle transport error"),
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
                debug!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} started",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                // we no actors start w/o names
                let actor_name = actor_cell.get_name().unwrap();

                warn!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_name,
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                // we no actors start w/o names
                let actor_name = actor_cell.get_name().unwrap();

                error!(
                    "Consumer_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_name,
                    actor_cell.get_id()
                );
            }

            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }

        Ok(())
    }
}
