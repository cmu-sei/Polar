use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef, Message};
use rkyv::{rancor::Error, Deserialize, Serialize};
use tracing::{error, info};
use std::marker::PhantomData;
use polar::DispatcherMessage;

use crate::types::GitlabData;


/// Dispatcher Definition
/// 
pub struct MessageDispatcher;


pub struct DispatcherState;


#[async_trait]
impl Actor for MessageDispatcher {
    type Msg = DispatcherMessage;
    type State = DispatcherState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ()
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(DispatcherState)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
        info!("{myself:?} started");
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            DispatcherMessage::Dispatch { message, topic } => {
                //TODO: implement dispatch logic to determine what kind of message we got here, 
                // safe to assume whatever message comes in contains data for our agent, we can discard any message that don't conform to our schema
                // Recall the naming convention for actors 
                // service:role:topic
                // where possible topics are users, projects, groups, etc. + config
                // Deserialize message data
                info!("Received dispatch message");

                match rkyv::from_bytes::<GitlabData, Error>(&message) {
                    Ok(message) => {
                        if let Some(consumer) = where_is(topic.clone()) {
                            
                            if let Err(e) = consumer.send_message(message) {
                                tracing::warn!("Error forwarding message. {e}");
                            }
                        }
                    }
                    Err(err) => error!("Failed to deserialize message: {:?}", err),
                }
                
            }

        
        }
        Ok(())        
    }
        
}

