use std::time::Duration;
use common::Todo;
use common::TodoData;
use ractor::registry::where_is;
use ractor::time::send_interval;
use reqwest::Client;
use rkyv::rancor;
use tracing::debug;
use tracing::error;
use tracing::field::debug;
use tracing::info;
use tracing::warn;
use ractor::rpc::call;
use ractor::rpc::CallResult;
use ractor::Actor;
use ractor::async_trait;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use cassini::client::*;
use utoipa::openapi::OpenApi;

use crate::BROKER_CLIENT_NAME;

pub struct ObserverSupervisor;

pub struct ObserverSupervisorState {
    max_registration_attempts: u32 // amount of times the supervisor will try to get the session_id from the client
}

pub struct ObserverSupervisorArgs {
    pub broker_addr: String,
    pub client_cert_file: String,
    pub client_private_key_file: String,
    pub ca_cert_file: String,
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
            max_registration_attempts: 5 
        };

        let (client, _) =  Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                bind_addr: args.broker_addr.clone(),
                ca_cert_file: args.ca_cert_file,
                client_cert_file: args.client_cert_file,
                private_key_file: args.client_private_key_file,
                registration_id: None
            }, myself.clone().into())
        .await.expect("Expected TCP client to start successfully.");

        // Set up an interval
        
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        //wait until we get a session id to start clients, try some configured amount of times every few seconds
        let mut attempts= 0; 
        loop { 
            
            attempts += 1;
            info!("Getting session data...");
            if let CallResult::Success(result) = call(&client, |reply| { TcpClientMessage::GetRegistrationId(reply) }, None).await.expect("Expected to call client!") {    
                if let Some(registration_id) = result {
                    let args = TodoObserverArgs {
                        registration_id: registration_id.clone()
                    };
    
                    let (todo_observer, _) = Actor::spawn_linked(Some("TODO_OBSERVER".to_string()), TodoObserver, args, myself.clone().into()).await.unwrap();
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
    
        Ok(state)
    }

    async fn post_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
        
        // get TODO open api spec

        let observer = where_is("TODO_OBSERVER".to_string()).unwrap();

        observer.send_message(TodoObserverMessage::GetApiSpec).unwrap();

        // start task to get todos ever few moments

        let _ = send_interval(Duration::from_secs(5), observer, || TodoObserverMessage::GetTodos);

        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        _: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
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

pub struct TodoObserver;

pub enum TodoObserverMessage {
    GetTodos,
    GetApiSpec
}

pub struct TodoObserverState {
    web_client: Client,
    registration_id: String,
}

pub struct TodoObserverArgs {
    registration_id: String,
}

#[async_trait]
impl Actor for TodoObserver {
    type Msg = TodoObserverMessage;
    type State = TodoObserverState;
    type Arguments = TodoObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: TodoObserverArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        
        let client = Client::new();
        
        let state = TodoObserverState {
            web_client: client,
            registration_id: args.registration_id
        };
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
        info!("{myself:?} Started");
        
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            TodoObserverMessage::GetApiSpec => {
                info!("Retreiving api spec");

                let resp = state.web_client.get("http://localhost:3000/api-docs/openapi.json").send().await
                .expect("Expected to contact example app");

    
                let spec = resp.text().await.unwrap();
                debug!("{spec}");
                
                let client = where_is(BROKER_CLIENT_NAME.to_owned()).expect("Expected to find TCP client");
                let payload =  rkyv::to_bytes::<rancor::Error>(&TodoData::OpenApiSpec(spec)).unwrap();
                 
                let message = cassini::ClientMessage::PublishRequest { topic: "todo:consumer:todos".to_string(), payload: payload.to_vec(), registration_id: Some(state.registration_id.clone()) };
                client.send_message(TcpClientMessage::Send(message)).unwrap();
            },
            TodoObserverMessage::GetTodos => {
                //send todos
                let todos = reqwest::get("http://localhost:3000/api/v1/todos")
                .await?
                .json::<Vec<Todo>>()
                .await?;
                let client = where_is(BROKER_CLIENT_NAME.to_owned()).expect("Expected to find TCP client");
                
                let payload =  rkyv::to_bytes::<rancor::Error>(&TodoData::Todo(todos)).unwrap();
                
                let message = TcpClientMessage::Send(cassini::ClientMessage::PublishRequest { topic: "todo:consumer:todos".to_string(), payload: payload.to_vec(), registration_id: Some(state.registration_id.clone()) });

                client.send_message(message).unwrap();
            },
        }
        Ok(())
    }

}
