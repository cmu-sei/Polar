use crate::types::GitlabEnvelope;
use polar::DispatcherMessage;
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use rkyv::rancor::Error;
use tracing::{info, warn};

/// Dispatcher Definition
/// The Dispatcher is a core component of each agent. Its function is to deserialize data from its binary
/// format into strong rust datatypes, and forward it to any interested actors.
pub struct MessageDispatcher;

impl MessageDispatcher {
    /// Helper function - does as the name says.
    /// It's pretty safe to assume whatever message comes in contains data for our Gitlab agent.
    /// We can just discard any message that don't conform to our expectations as bad data.
    /// The incoming topic string will inform us of which actor is responsible for handling the message.
    /// Other agents may want to implement DLQ for when consumers aren't present and may return.
    /// For now, we don't need to bother since we're getting new data all the time from gitlab.
    pub fn deserialize_and_dispatch(message: Vec<u8>, topic: String) {
        match rkyv::from_bytes::<GitlabEnvelope, Error>(&message) {
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

pub struct DispatcherState;

#[async_trait]
impl Actor for MessageDispatcher {
    type Msg = DispatcherMessage;
    type State = DispatcherState;
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(DispatcherState)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
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
                MessageDispatcher::deserialize_and_dispatch(message, topic);
            }
        }
        Ok(())
    }
}
