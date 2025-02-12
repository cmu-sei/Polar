use core::error;
use std::time::Duration;

use tracing::error;
use tracing::{debug, info, warn};
use ractor::call;
use ractor::rpc::call;
use ractor::rpc::CallResult;
use ractor::Actor;
use ractor::async_trait;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use cassini::client::*;

use crate::groups::GitlabGroupObserver;
use crate::projects::GitlabProjectObserver;
use crate::runners::GitlabRunnerObserver;
use crate::users::GitlabUserObserver;
use crate::GitlabObserverArgs;
use crate::BROKER_CLIENT_NAME;
use crate::GITLAB_USERS_OBSERVER;

pub struct ObserverSupervisor;

pub struct ObserverSupervisorState {
    max_registration_attempts: u32 // amount of times the supervisor will try to get the session_id from the client
}


pub struct ObserverSupervisorArgs {
    pub broker_addr: String,
    pub client_cert_file: String,
    pub client_private_key_file: String,
    pub ca_cert_file: String,
    pub gitlab_endpoint: String,
    pub gitlab_token: Option<String>,
}

#[async_trait]
impl Actor for ObserverSupervisor {
    type Msg = ();
    type State = ObserverSupervisorState;
    type Arguments = ObserverSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ObserverSupervisorArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        
        let state = ObserverSupervisorState {
            max_registration_attempts: 5 //TODO: take from args
        };

        let client_started_result =  Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                bind_addr: args.broker_addr.clone(),
                ca_cert_file: args.ca_cert_file,
                client_cert_file: args.client_cert_file,
                private_key_file: args.client_private_key_file,
                registration_id: None
            }, myself.clone().into())
            .await;

        //TODO: Expect client to start, because if it doesn't we're SOL, don't bother matching.
        match client_started_result {
            Ok((client, _)) => {
                // Set up an interval
                //TODO: make configurable
                let mut interval = tokio::time::interval(Duration::from_millis(500));
                //wait until we get a session id to start clients, try some configured amount of times every few seconds
                let mut attempts= 0; 
                loop {
                    
                    attempts += 1;
                    info!("Getting session data...");
                    if let CallResult::Success(result) = call(&client, |reply| { TcpClientMessage::GetRegistrationId(reply) }, None).await.expect("Expected to call client!") {    
                        if let Some(registration_id) = result {
                            let args = GitlabObserverArgs {
                                gitlab_endpoint: args.gitlab_endpoint.clone(),
                                token: args.gitlab_token.clone(),
                                registration_id: registration_id.clone()
                            };
            
                            //TODO: start observers based off of some configuration
                            if let Err(e) = Actor::spawn_linked(Some(GITLAB_USERS_OBSERVER.to_string()), GitlabUserObserver, args.clone(), myself.clone().into()).await { warn!( "failed to start users observer {e}") }
                            if let Err(e) = Actor::spawn_linked(Some("GITLAB_PROJECT_OBSERVER".to_string()), GitlabProjectObserver, args.clone(), myself.clone().into()).await { warn!( "failed to start project observer {e}") }
                            if let Err(e) = Actor::spawn_linked(Some("GITLAB_GROUP_OBSERVER".to_string()), GitlabGroupObserver, args.clone(), myself.clone().into()).await { warn!( "failed to start group observer {e}") }
                            if let Err(e) = Actor::spawn_linked(Some("GITLAB_RUNNER_OBSERVER".to_string()), GitlabRunnerObserver, args.clone(), myself.clone().into()).await { warn!( "failed to start runner observer {e}") }
                            
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

    async fn post_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
        //TODO: Implement scheduling logic via clockwerk or some other means based off of configuration values within the state
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        _: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        //TODO: Implement some message handling here, handle cases where the scheudler may request a certain resource be observed, like users, projects, etc.
        Ok(())
    }

    async fn handle_supervisor_evt(&self, _: ActorRef<Self::Msg>, msg: SupervisionEvent, _: &mut Self::State) -> Result<(), ActorProcessingErr> {
        
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                info!("OBSERVER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}", actor_cell.get_name(), actor_cell.get_id());
            },
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!("OBSERVER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}", actor_cell.get_name(), actor_cell.get_id());
            },
            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }    
        
        Ok(())
    }
}