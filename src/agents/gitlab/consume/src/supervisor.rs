use std::time::Duration;

use crate::groups::GitlabGroupConsumer;
use crate::meta::MetaConsumer;
use crate::pipelines::GitlabPipelineConsumer;
use crate::projects::GitlabProjectConsumer;
use crate::repositories::GitlabRepositoryConsumer;
use crate::runners::GitlabRunnerConsumer;
use crate::users::GitlabUserConsumer;
use crate::GitlabConsumerArgs;
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
impl ConsumerSupervisor {
    /// Helper to try restarting an actor at least 10 times using an exponential backoff strategy.
    /// TODO: This function doesn't seem to be helping in its current implementation. Restarts aren't real restarts.
    /// Helper to try restarting an actor at least 10 times using an exponential backoff strategy.
    async fn restart_actor(
        actor_name: String,
        supervisor: ActorRef<SupervisorMessage>,
        graph_config: neo4rs::Config,
    ) -> Result<ActorRef<GitlabData>, String> {
        // Configure the exponential backoff policy
        let mut backoff = ExponentialBackoff::default();
        backoff.initial_interval = Duration::from_secs(3);
        backoff.max_interval = Duration::from_secs(300);
        backoff.max_elapsed_time = Some(Duration::from_secs(3000)); // ~10 tries

        let mut count = 0;

        while let Some(duration) = backoff.next_backoff() {
            count += 1;

            let registration_id = ConsumerSupervisor::get_registration_id()
                .await
                .expect("Expected agent to be registered.");

            let args = GitlabConsumerArgs {
                registration_id,
                graph_config: graph_config.clone(),
            };

            debug!("Restarting actor: {actor_name}, attempt: {count}");

            // match on actor_name to spawn the right consumer
            let spawn_result = match actor_name.as_str() {
                USER_CONSUMER_TOPIC => {
                    Actor::spawn_linked(
                        Some(USER_CONSUMER_TOPIC.to_string()),
                        GitlabUserConsumer,
                        args.clone(),
                        supervisor.clone().into(),
                    )
                    .await
                }
                PROJECTS_CONSUMER_TOPIC => {
                    Actor::spawn_linked(
                        Some(PROJECTS_CONSUMER_TOPIC.to_string()),
                        GitlabProjectConsumer,
                        args.clone(),
                        supervisor.clone().into(),
                    )
                    .await
                }
                GROUPS_CONSUMER_TOPIC => {
                    Actor::spawn_linked(
                        Some(GROUPS_CONSUMER_TOPIC.to_string()),
                        GitlabGroupConsumer,
                        args.clone(),
                        supervisor.clone().into(),
                    )
                    .await
                }
                RUNNERS_CONSUMER_TOPIC => {
                    Actor::spawn_linked(
                        Some(RUNNERS_CONSUMER_TOPIC.to_string()),
                        GitlabRunnerConsumer,
                        args.clone(),
                        supervisor.clone().into(),
                    )
                    .await
                }
                _ => return Err(format!("Unsupported actor name: {actor_name}")),
            };

            match spawn_result {
                Ok(_) => break,
                Err(_) => {
                    warn!(
                        "Failed to start actor {actor_name}, retrying in {:?} (attempt {count})",
                        duration
                    );
                    tokio::time::sleep(duration).await;
                }
            }
        }

        match where_is(actor_name.clone()) {
            Some(actor) => Ok(actor.into()),
            None => Err(format!(
                "Failed to start actor {actor_name} after {count} attempt(s)!"
            )),
        }
    }

    pub async fn get_registration_id() -> Option<String> {
        let client =
            where_is(BROKER_CLIENT_NAME.to_string()).expect("Expected to find TCP Client.");

        match call(
            &client,
            |reply| TcpClientMessage::GetRegistrationId(reply),
            None,
        )
        .await
        .expect("Expected to call TCP Client")
        {
            CallResult::Success(registration_id) => registration_id,
            _ => panic!("Couldn't contact TCP Client"),
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

        let (_broker_client, _) = Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config: client_config,
                registration_id: None,
                events_output: Some(events_output),
                event_handler: None,
            },
            myself.clone().into(),
        )
        .await?;

        let graph_config = get_neo_config()?;
        let state = ConsumerSupervisorState {
            graph_config: graph_config,
        };
        // start dispatcher
        // let (dispatcher, _) = Actor::spawn_linked(
        //     Some(DISPATCH_ACTOR.to_string()),
        //     MessageDispatcher,
        //     (),
        //     myself.clone().into(),
        // )
        // .await?;

        // queue_output.subscribe(dispatcher, |(payload, topic)| {
        //     Some(DispatcherMessage::Dispatch {
        //         message: payload,
        //         topic,
        //     })
        // });

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
                ClientEvent::Registered { registration_id } => {
                    let args = GitlabConsumerArgs {
                        registration_id,
                        graph_config: state.graph_config.clone(),
                    };

                    if let Err(e) = Actor::spawn_linked(
                        Some(METADATA_CONSUMER_TOPIC.to_string()),
                        MetaConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start meta consumer. {e}");
                        myself.stop(None);
                    }
                    if let Err(e) = Actor::spawn_linked(
                        Some(USER_CONSUMER_TOPIC.to_string()),
                        GitlabUserConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start users consumer. {e}");
                        myself.stop(None);
                    }
                    if let Err(e) = Actor::spawn_linked(
                        Some(GROUPS_CONSUMER_TOPIC.to_string()),
                        GitlabGroupConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start groups consumer. {e}");
                        myself.stop(None);
                    }
                    if let Err(e) = Actor::spawn_linked(
                        Some(RUNNERS_CONSUMER_TOPIC.to_string()),
                        GitlabRunnerConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start runners consumer. {e}");
                        myself.stop(None);
                    }
                    if let Err(e) = Actor::spawn_linked(
                        Some(PROJECTS_CONSUMER_TOPIC.to_string()),
                        GitlabProjectConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start projects consumer. {e}");
                        myself.stop(None);
                    }
                    if let Err(e) = Actor::spawn_linked(
                        Some(PIPELINE_CONSUMER_TOPIC.to_string()),
                        GitlabPipelineConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start pipeline consumer. {e}");
                        myself.stop(None);
                    }
                    if let Err(e) = Actor::spawn_linked(
                        Some(REPOSITORY_CONSUMER_TOPIC.to_string()),
                        GitlabRepositoryConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start pipeline consumer. {e}");
                        myself.stop(None);
                    }
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    ConsumerSupervisor::deserialize_and_dispatch(topic, payload);
                }
                ClientEvent::TransportError { ..} => todo!("Handle transport error"),
                ClientEvent::ControlResponse { .. } => {
                    // ignore
                },
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

                match ConsumerSupervisor::restart_actor(
                    actor_name.clone(),
                    myself.clone(),
                    state.graph_config.clone(),
                )
                .await
                {
                    Ok(actor) => {
                        info!("Restarted actor {0:?}:{1:?}", actor_name, actor.get_id())
                    }
                    Err(e) => {
                        error!("Failed to recover actor: {e}");
                        myself.stop(Some(e))
                    }
                }
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                // we no actors start w/o names
                let actor_name = actor_cell.get_name().unwrap();

                error!(
                    "Consumer_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_name,
                    actor_cell.get_id()
                );
                match ConsumerSupervisor::restart_actor(
                    actor_name.clone(),
                    myself.clone(),
                    state.graph_config.clone(),
                )
                .await
                {
                    Ok(actor) => {
                        info!("Restarted actor {0:?}:{1:?}", actor_name, actor.get_id())
                    }
                    Err(e) => {
                        error!("Failed to recover actor: {e}");
                        myself.stop(Some(e))
                    }
                }
            }

            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }

        Ok(())
    }
}
