use std::time::Duration;

use cassini::client::*;
use common::dispatch::MessageDispatcher;
use common::types::GitlabData;
use common::GROUPS_CONSUMER_TOPIC;
use common::PROJECTS_CONSUMER_TOPIC;
use common::RUNNERS_CONSUMER_TOPIC;
use common::USER_CONSUMER_TOPIC;
use exponential_backoff::Backoff;
use polar::DISPATCH_ACTOR;
use ractor::async_trait;
use ractor::registry::where_is;
use ractor::rpc::call;
use ractor::rpc::CallResult;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::get_neo_config;
use crate::groups::GitlabGroupConsumer;
use crate::projects::GitlabProjectConsumer;
use crate::runners::GitlabRunnerConsumer;
use crate::users::GitlabUserConsumer;
use crate::GitlabConsumerArgs;
use crate::BROKER_CLIENT_NAME;

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    max_registration_attempts: u32, // amount of times the supervisor will try to get the session_id from the client
    graph_config: neo4rs::Config
}

pub enum ConsumerSupervisorMessage {
    TopicMessage { topic: String, payload: String },
}

pub struct ConsumerSupervisorArgs {
    pub client_config: cassini::TCPClientConfig,
    pub graph_config: neo4rs::Config
}


impl ConsumerSupervisor {

    /// Helper to try restarting an actor at least 10 times using an exponential backoff strategy.
    async fn restart_actor(actor_name: String, supervisor: ActorRef<ConsumerSupervisorMessage>, graph_config: neo4rs::Config) -> Result<ActorRef<GitlabData>, String> {
        // Make attempts to restart
        // This section could be improved somewhat. The idea of using a hashmap of callback functions was suggested
        // but this is far more readable. Various typing errors were discovered trying to implement the hashmap.
        // Also, it'd be more ideal for this consumer to stay alive indefinitely, continuously retrying but that doesn't seem supported w/ this crate.
        // If the grpah isn't back after 5-10 minutes we probably have a serious issue anyway.
        
        let attempts = 10;
        let min = Duration::from_secs(3);
        let max = Duration::from_secs(300);
        let mut count = 0;

        for duration in Backoff::new(attempts, min, max) {
            count += 1;
            //try starting the actor based on the name
            
            let registration_id = ConsumerSupervisor::get_registration_id().await.expect("Expected agent to be registered.");
            let args = GitlabConsumerArgs { registration_id , graph_config: graph_config.clone() };

            debug!("Restarting actor: {actor_name}, attempt: {count}");

            if actor_name == USER_CONSUMER_TOPIC {
                match Actor::spawn_linked(
                    Some(USER_CONSUMER_TOPIC.to_string()),
                    GitlabUserConsumer,
                    args.clone(),
                    supervisor.clone().into(),
                )
                .await
                {
                     
                    Ok(_) => break,
                    // if we have an issue starting the actor, just sleep and try again later
                    Err(_) => match duration { Some(duration) => tokio::time::sleep(duration).await , None => break }
                }  
            }
            else if actor_name == PROJECTS_CONSUMER_TOPIC {
                match Actor::spawn_linked(
                    Some(PROJECTS_CONSUMER_TOPIC.to_string()),
                    GitlabProjectConsumer,
                    args.clone(),
                    supervisor.clone().into(),
                )
                .await
                {
                     
                    Ok(_) => break,
                    // if we have an issue starting the actor, just sleep and try again later
                    Err(_) => match duration { Some(duration) => tokio::time::sleep(duration).await , None => break }
                }  
            }
            else if actor_name == GROUPS_CONSUMER_TOPIC {
                match Actor::spawn_linked(
                    Some(GROUPS_CONSUMER_TOPIC.to_string()),
                    GitlabGroupConsumer,
                    args.clone(),
                    supervisor.clone().into(),
                )
                .await
                {
                     
                    Ok(_) => break,
                    // if we have an issue starting the actor, just sleep and try again later
                    Err(_) => match duration { Some(duration) => tokio::time::sleep(duration).await , None => break }
                }  
            }
            else if actor_name == RUNNERS_CONSUMER_TOPIC {
                match Actor::spawn_linked(
                    Some(RUNNERS_CONSUMER_TOPIC.to_string()),
                    GitlabRunnerConsumer,
                    args.clone(),
                    supervisor.clone().into(),
                )
                .await
                {
                     
                    Ok(_) => break,
                    // if we have an issue starting the actor, just sleep and try again later
                    Err(_) => match duration { Some(duration) => tokio::time::sleep(duration).await , None => break }
                }  
            } 
        }
    
        match where_is(actor_name.clone()) {
            Some(actor) => Ok(actor.into()), None => Err(format!("Failed to start actor {actor_name} after {count} attempt(s)!"))
        }
    }
    
    pub async fn get_registration_id() -> Option<String> {
        let client = where_is(BROKER_CLIENT_NAME.to_string()).expect("Expected to find TCP Client.");
        
        match call(&client, |reply| {
            TcpClientMessage::GetRegistrationId(reply)
        }, None).await.expect("Expected to call TCP Client") {
            CallResult::Success(registration_id) => registration_id,
            _ => panic!("Couldn't contact TCP Client")   
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

        let state = ConsumerSupervisorState {
            max_registration_attempts: 5,
            graph_config: get_neo_config()
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

        match Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config: args.client_config,
                registration_id: None,
            },
            myself.clone().into(),
        )
        .await
        {
            Ok((client, _)) => {
                //when client starts successfully, start workers

                // Set up an interval
                let mut interval = tokio::time::interval(Duration::from_secs(5));
                //wait until we get a session id to start clients, try some configured amount of times every few seconds
                let mut attempts = 0;
                loop {
                    attempts += 1;
                    info!("Getting session data...");
                    if let CallResult::Success(result) = call(
                        &client,
                        |reply| TcpClientMessage::GetRegistrationId(reply),
                        None,
                    )
                    .await
                    .expect("Expected to call client!")
                    {
                        // if we got got registered, start actors.
                        // At present, the only reason consumer should fail on startup is if they're in an insane state.
                        // In this case, failing to find the GRAPH_CA_CERT file. In which case, there' snothing we can really do.
                        // Should that happen, we should just log the error and stop
                        if let Some(registration_id) = result {
                            let args = GitlabConsumerArgs { registration_id, graph_config: get_neo_config() };

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

                            break;
                        } else if attempts < state.max_registration_attempts {
                            warn!("Failed to get session data. Retrying.");
                        } else if attempts >= state.max_registration_attempts {
                            error!("Failed to retrieve session data! timed out");
                            myself.stop(Some(
                                "Failed to retrieve session data! timed out".to_string(),
                            ));
                        }
                    }
                    interval.tick().await;
                }
            }
            Err(e) => {
                error!("{e}");
                myself.stop(None);
            }
        }

        Ok(state)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        _: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
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

               match ConsumerSupervisor::restart_actor(actor_name.clone(), myself.clone(), state.graph_config.clone()).await {
                    Ok(actor) => {
                        info!("Restarted actor {0:?}:{1:?}", 
                        actor_name,
                        actor.get_id())
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
                match ConsumerSupervisor::restart_actor(actor_name.clone(), myself.clone(), state.graph_config.clone()).await {
                    Ok(actor) => {
                        info!("Restarted actor {0:?}:{1:?}", 
                        actor_name,
                        actor.get_id())
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
