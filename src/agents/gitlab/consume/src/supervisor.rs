use log::debug;
use log::error;
use log::info;
use log::warn;
use ractor::rpc::call;
use ractor::Actor;
use ractor::async_trait;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use cassini::client::*;

use crate::BROKER_CLIENT_NAME;

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState;


pub struct ConsumerSupervisorArgs {
    pub broker_addr: String,
    pub client_cert_file: String,
    pub client_private_key_file: String,
    pub ca_cert_file: String,    
}

#[async_trait]
impl Actor for ConsumerSupervisor {
    type Msg = ();
    type State = ConsumerSupervisorState;
    type Arguments = ConsumerSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ConsumerSupervisorArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        
        let state = ConsumerSupervisorState;

        match Actor::spawn_linked(Some(BROKER_CLIENT_NAME.to_string()), TcpClientActor, TcpClientArgs {
            bind_addr: args.broker_addr.clone(),
            ca_cert_file: args.ca_cert_file,
            client_cert_file: args.client_cert_file,
            private_key_file: args.client_private_key_file,
            registration_id: None
            
        }, myself.clone().into()).await {
            Ok((client, _)) => {
                //when client starts, successfully, start workers
                //TODO: start users actor
                let id = call(&client, |reply| { TcpClientMessage::GetRegistrationId(reply) }, None)
                .await.expect("Expected client to register successfully.").unwrap();
                
                //TODO: Start consumers
                //TODO: Read some configuration from supervisor to influence how this is done
                

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

    async fn handle_supervisor_evt(&self, myself: ActorRef<Self::Msg>, msg: SupervisionEvent, _: &mut Self::State) -> Result<(), ActorProcessingErr> {
        
        match msg {
            SupervisionEvent::ActorStarted(actor_cell) => {
                info!("Consumer_SUPERVISOR: {0:?}:{1:?} started", actor_cell.get_name(), actor_cell.get_id());
            },
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                info!("Consumer_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}", actor_cell.get_name(), actor_cell.get_id());
            },
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!("Consumer_SUPERVISOR: {0:?}:{1:?} failed! {e:?}", actor_cell.get_name(), actor_cell.get_id());
            },
            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }    
        
        Ok(())
    }
}