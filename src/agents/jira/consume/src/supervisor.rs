use std::time::Duration;

use cassini::client::*;
use jira_common::dispatch::MessageDispatcher;
use jira_common::types::JiraData;
use jira_common::JIRA_PROJECTS_CONSUMER_TOPIC;
use exponential_backoff::Backoff;
use polar::DISPATCH_ACTOR;
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

use crate::get_neo_config;
use crate::projects::JiraProjectConsumer;
use crate::JiraConsumerArgs;
use crate::BROKER_CLIENT_NAME;

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    graph_config: neo4rs::Config,
}

pub enum ConsumerSupervisorMessage {
    /// TCP client successfully registered with the broker
    ClientRegistered(String),
}

pub struct ConsumerSupervisorArgs {
    pub client_config: cassini::TCPClientConfig,
    pub graph_config: neo4rs::Config,
}

impl ConsumerSupervisor {
    /// Helper to try restarting an actor at least 10 times using an exponential backoff strategy.
    async fn restart_actor(
        actor_name: String,
        supervisor: ActorRef<ConsumerSupervisorMessage>,
        graph_config: neo4rs::Config,
    ) -> Result<ActorRef<JiraData>, String> {
        // Make attempts to restart
        // This section could be improved somewhat. The idea of using a hashmap of callback functions was suggested
        // but this is far more readable. Various typing errors were discovered trying to implement the hashmap.
        // Also, it'd be more ideal for this consumer to stay alive indefinitely, continuously retrying but that doesn't seem supported w/ this crate.
        // If the graph isn't back after 5-10 minutes we probably have a serious issue anyway.

        let attempts = 10;
        let min = Duration::from_secs(3);
        let max = Duration::from_secs(300);
        let mut count = 0;

        for duration in Backoff::new(attempts, min, max) {
            count += 1;
            //try starting the actor based on the name

            let registration_id = ConsumerSupervisor::get_registration_id()
                .await
                .expect("Expected agent to be registered.");
            let args = JiraConsumerArgs {
                registration_id,
                graph_config: graph_config.clone(),
            };

            debug!("Restarting actor: {actor_name}, attempt: {count}");

            if actor_name == JIRA_PROJECTS_CONSUMER_TOPIC {
                match Actor::spawn_linked(
                    Some(JIRA_PROJECTS_CONSUMER_TOPIC.to_string()),
                    JiraProjectConsumer,
                    args.clone(),
                    supervisor.clone().into(),
                )
                .await
                {
                    Ok(_) => break,
                    // if we have an issue starting the actor, just sleep and try again later
                    Err(_) => match duration {
                        Some(duration) => tokio::time::sleep(duration).await,
                        None => break,
                    },
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
    type Msg = ConsumerSupervisorMessage;
    type State = ConsumerSupervisorState;
    type Arguments = ConsumerSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ConsumerSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let output_port = std::sync::Arc::new(OutputPort::default());

        //subscribe
        output_port.subscribe(myself.clone(), |message| {
            Some(ConsumerSupervisorMessage::ClientRegistered(message))
        });

        match Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config: args.client_config,
                registration_id: None,
                output_port,
            },
            myself.clone().into(),
        )
        .await
        {
            Ok(_) => {
                let state = ConsumerSupervisorState {
                    graph_config: get_neo_config(),
                };

                // start dispatcher
                let _ = Actor::spawn_linked(
                    Some(DISPATCH_ACTOR.to_string()),
                    MessageDispatcher,
                    (),
                    myself.clone().into(),
                )
                .await
                .expect("Expected to start dispatcher");
                Ok(state)
            }
            Err(e) => Err(ActorProcessingErr::from(e)),
        }
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ConsumerSupervisorMessage::ClientRegistered(registration_id) => {
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
            }
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
