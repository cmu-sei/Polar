use std::time::Duration;

use crate::get_neo_config;
use crate::groups::JiraGroupConsumer;
use crate::issues::JiraIssueConsumer;
use crate::projects::JiraProjectConsumer;
use crate::users::JiraUserConsumer;
use crate::JiraConsumerArgs;
use crate::BROKER_CLIENT_NAME;
use cassini_backoff::{Backoff, ExponentialBackoff};
use cassini_client::TCPClientConfig;
use cassini_client::{TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent;
use jira_common::types::JiraData;
use jira_common::JIRA_GROUPS_CONSUMER_TOPIC;
use jira_common::JIRA_ISSUES_CONSUMER_TOPIC;
use jira_common::JIRA_PROJECTS_CONSUMER_TOPIC;
use jira_common::JIRA_USERS_CONSUMER_TOPIC;
use polar::{Supervisor, SupervisorMessage};
use ractor::async_trait;
use ractor::registry::where_is;
use ractor::rpc::call;
use ractor::rpc::CallResult;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::OutputPort;
use ractor::SupervisionEvent;
use tokio::task::JoinHandle;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    graph_config: neo4rs::Config,
}
impl Supervisor for ConsumerSupervisor {
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>) {
        match rkyv::from_bytes::<JiraData, rkyv::rancor::Error>(&payload) {
            Ok(message) => {
                if let Some(consumer) = where_is(topic.clone()) {
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
    async fn restart_actor(
        actor_name: String,
        supervisor: ActorRef<SupervisorMessage>,
        graph_config: neo4rs::Config,
    ) -> Result<ActorRef<JiraData>, String> {
        // Set up an exponential backoff policy
        let mut backoff = ExponentialBackoff::default();
        backoff.initial_interval = Duration::from_secs(3);
        backoff.max_interval = Duration::from_secs(300);
        backoff.max_elapsed_time = Some(Duration::from_secs(3000)); // roughly 10 attempts

        let mut count = 0;

        while let Some(duration) = backoff.next_backoff() {
            count += 1;

            let registration_id = ConsumerSupervisor::get_registration_id()
                .await
                .expect("Expected agent to be registered.");

            let args = JiraConsumerArgs {
                registration_id,
                graph_config: graph_config.clone(),
            };

            debug!("Restarting actor: {actor_name}, attempt: {count}");

            let spawn_result: Result<(ActorRef<JiraData>, JoinHandle<()>), ActorProcessingErr> =
                if actor_name == JIRA_ISSUES_CONSUMER_TOPIC {
                    Actor::spawn_linked(
                        Some(JIRA_ISSUES_CONSUMER_TOPIC.to_string()),
                        JiraIssueConsumer,
                        args.clone(),
                        supervisor.clone().into(),
                    )
                    .await
                    .map_err(|e| ActorProcessingErr::from(format!("{e}")))
                } else {
                    Err(ActorProcessingErr::from("Unsupported actor name"))
                };

            match spawn_result {
                Ok(_) => break,
                Err(_) => {
                    warn!(
                        "Failed to start actor {actor_name}, retrying in {:?}",
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

        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        let client_config = TCPClientConfig::new()?;

        let (_client, _) = Actor::spawn_linked(
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

        let state = ConsumerSupervisorState {
            graph_config: get_neo_config(),
        };

        Ok(state)
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { registration_id } => {
                    let args = JiraConsumerArgs {
                        registration_id,
                        graph_config: get_neo_config(),
                    };

                    if let Err(e) = Actor::spawn_linked(
                        Some(JIRA_PROJECTS_CONSUMER_TOPIC.to_string()),
                        JiraProjectConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start projects consumer. {e}");
                        myself.stop(None);
                    }
                    if let Err(e) = Actor::spawn_linked(
                        Some(JIRA_GROUPS_CONSUMER_TOPIC.to_string()),
                        JiraGroupConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start groups consumer. {e}");
                        myself.stop(None);
                    }
                    if let Err(e) = Actor::spawn_linked(
                        Some(JIRA_USERS_CONSUMER_TOPIC.to_string()),
                        JiraUserConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start users consumer. {e}");
                        myself.stop(None);
                    }

                    if let Err(e) = Actor::spawn_linked(
                        Some(JIRA_ISSUES_CONSUMER_TOPIC.to_string()),
                        JiraIssueConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start issues consumer. {e}");
                        myself.stop(None);
                    }
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    ConsumerSupervisor::deserialize_and_dispatch(topic, payload);
                }
                ClientEvent::TransportError { reason: _ } => todo!(),
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
                info!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} started",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                // we no actors start w/o names
                let actor_name = actor_cell.get_name().unwrap();

                info!(
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

                warn!(
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
