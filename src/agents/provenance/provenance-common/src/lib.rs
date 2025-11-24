use polar::{DispatcherMessage, ProvenanceEvent};
use ractor::{async_trait, registry::where_is, rpc::cast, Actor, ActorProcessingErr, ActorRef};
use rkyv::{from_bytes, rancor};
use tracing::{debug, error, trace};
pub const PROVENANCE_LINKER_NAME: &str = "polar.provenance.linker";
pub const LINKER_SUPERVISOR_NAME: &str = "polar.provenance.linker.supervisor";
pub const RESOLVER_CLIENT_NAME: &str = "polar.provenance.resolver";
pub const RESOLVER_SUPERVISOR_NAME: &str = "polar.provenance.resolver.supervisor";

/// Dispatcher Definition
/// The Dispatcher is a core component of each agent. Its function is to deserialize data from its binary
/// format into strong rust datatypes, and forward it to any interested actors.
pub struct MessageDispatcher;

impl MessageDispatcher {
    /// Helper function - does as the name says.
    /// It's pretty safe to assume whatever message comes in contains data for our agent.
    /// We can just discard any message that don't conform to our expectations as bad data.
    /// The incoming topic string will inform us of which actor is responsible for handling the message.
    pub fn deserialize_and_dispatch(payload: Vec<u8>, recipient: &str) {
        //try deserializng payload to a provenance event and forward
        match from_bytes::<ProvenanceEvent, rancor::Error>(&payload) {
            Ok(message) => {
                // Forward the message to the appropriate actor
                match where_is(recipient.to_string()) {
                    Some(actor) => {
                        trace!("Forwarding message to actor: {}", recipient);
                        cast(&actor, message)
                            .map_err(|e| error!("Failed to cast message: {}", e))
                            .ok();
                    }
                    None => error!("Failed to find actor: {}", recipient),
                };
            }
            Err(e) => error!("Failed to deserialize message: {}", e),
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
        debug!("{myself:?} started");
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        trace!("Handling message: {:?}", message);
        match message {
            // TODO: We don't really use the topic here, but in the future, we might
            DispatcherMessage::Dispatch { message, topic } => {
                MessageDispatcher::deserialize_and_dispatch(message, &topic);
            }
        }
        Ok(())
    }
}
