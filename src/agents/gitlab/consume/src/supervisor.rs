use std::time::Duration;

use log::debug;
use log::error;
use log::info;
use log::warn;
use ractor::rpc::call;
use ractor::rpc::CallResult;
use ractor::Actor;
use ractor::async_trait;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use cassini::client::*;

use crate::users::GitlabUserConsumer;
use crate::GitlabConsumerArgs;
use crate::BROKER_CLIENT_NAME;
use crate::GITLAB_USER_CONSUMER;

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    max_registration_attempts: u32 // amount of times the supervisor will try to get the session_id from the client
}

pub enum ConsumerSupervisorMessage {
    TopicMessage {
        topic: String,
        payload: String
    }
}

pub struct ConsumerSupervisorArgs {
    pub broker_addr: String,
    pub client_cert_file: String,
    pub client_private_key_file: String,
    pub ca_cert_file: String,    
}

#[async_trait]
impl Actor for ConsumerSupervisor {
    type Msg = ConsumerSupervisorMessage;
    type State = ConsumerSupervisorState;
    type Arguments = ConsumerSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ConsumerSupervisorArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        
        let state = ConsumerSupervisorState { max_registration_attempts: 5 };

        match Actor::spawn_linked(Some(BROKER_CLIENT_NAME.to_string()), TcpClientActor, TcpClientArgs {
            bind_addr: args.broker_addr.clone(),
            ca_cert_file: args.ca_cert_file,
            client_cert_file: args.client_cert_file,
            private_key_file: args.client_private_key_file,
            registration_id: None
            
        }, myself.clone().into()).await {
            Ok((client, _)) => {
                //when client starts, successfully, start workers
                // Set up an interval
                //TODO: make configurable
                let mut interval = tokio::time::interval(Duration::from_secs(5));
                //wait until we get a session id to start clients, try some configured amount of times every few seconds
                let mut attempts= 0; 
                loop {
                    
                    attempts += 1;
                    info!("Getting session data...");
                    if let CallResult::Success(result) = call(&client, |reply| { TcpClientMessage::GetRegistrationId(reply) }, None).await.expect("Expected to call client!") {    
                        if let Some(registration_id) = result {

                            let args = GitlabConsumerArgs { registration_id };
                            
                            if let Err(e) = Actor::spawn_linked(Some(GITLAB_USER_CONSUMER.to_string()), GitlabUserConsumer, args.clone(), myself.clone().into()).await { warn!( "failed to start users observer {e}") }
                                                    
                            break;
                        } else if attempts < state.max_registration_attempts {
                          warn!("Failed to get session data. Retrying.");
                        } else if attempts >= state.max_registration_attempts{
                            error!("Failed to retrieve session data! timed out");
                            myself.stop(Some("Failed to retrieve session data! timed out".to_string()));
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
        message: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        
        match message {
            _ => todo!()
        }
        Ok(())
    }

    async fn handle_supervisor_evt(&self, myself: ActorRef<Self::Msg>, msg: SupervisionEvent, _: &mut Self::State) -> Result<(), ActorProcessingErr> {
        
        match msg {
            SupervisionEvent::ActorStarted(actor_cell) => {
                info!("CONSUMER_SUPERVISOR: {0:?}:{1:?} started", actor_cell.get_name(), actor_cell.get_id());
            },
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                info!("CONSUMER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}", actor_cell.get_name(), actor_cell.get_id());
            },
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!("Consumer_SUPERVISOR: {0:?}:{1:?} failed! {e:?}", actor_cell.get_name(), actor_cell.get_id());
            },
            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }    
        
        Ok(())
    }
}