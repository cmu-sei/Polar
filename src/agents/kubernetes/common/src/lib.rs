use polar::DispatcherMessage;
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use rkyv::rancor::Error;
use tracing::{debug, info, warn};
use serde::{Serialize, Deserialize};
use serde_json::Value;
use std::fmt::Debug;
use kube::api::ObjectList;


#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RawKubeEvent {
    /// e.g. "Pod", "ConfigMap", "Service"
    pub kind: String,
    /// e.g. "Applied", "Deleted", "InitApply"
    pub action: String,
    /// the raw JSON of the object
    pub object: Value,
    // pub namespace: Option<String>,
}


/// Messages intended to be serialized and set across the broker boundary to consumers for processing
/// Generics make things 10x simpler here, as all types from the k8s_openapi crate can be serialized with serde.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KubeMessage {
    /// Observer read a batch of resources during initial sync with the server.
    ResourceBatch {
        kind: String,
        resources: Value,
    },
    /// A new or updated resource has been observed
    /// 
    ResourceApplied {
        kind: String,
        resource: Value,
    },

    /// A resource has been deleted
    ResourceDeleted {
        kind: String,
        resource: Value,
    },

    /// Indicates that the watcher is resetting and will re-list resources
    ResyncStarted {
        kind: String,
        resource: Value
    },

    // /// A resource has been observed during re-sync (initial apply)
    // ResyncResource {
    //     kind: String
    //     resource: Value,
    // },

    // /// Resync has completed and all InitApply resources have been listed
    // ResyncCompleted {
    //     namespace: Option<String>,
    //     resource_kind: String,
    // },
}


/// Dispatcher Definition
/// The Dispatcher is a core component of each agent. Its function is to deserialize data from its binary
/// format into strong rust datatypes, and forward it to any interested actors.
pub struct MessageDispatcher;

impl MessageDispatcher {
    /// Helper function - does as the name says.
    /// It's pretty safe to assume whatever message comes in contains data for our agent.
    /// We can just discard any message that don't conform to our expectations as bad data.
    /// The incoming topic string will inform us of which actor is responsible for handling the message.
    pub fn deserialize_and_dispatch(message: Vec<u8>, topic: String) {
        //TODO: we have to deserialize to hard types, but we use a generic message, so maybe we can just match based on the topic and deserialize to a type that way?
        match serde_json::from_slice::<RawKubeEvent>(&message) {
            Ok(event) => {
                // try to lookup resource by kind, if an actor is missing, drop the message
                match where_is(format!("kubernetes.cluster.consumer.{}", event.kind)) {
                    Some(consumer) => {
                        debug!("Received data for {}", consumer.get_name().unwrap());
                        let message = match event.action.as_str() {
                            "Applied" => {
                                KubeMessage::ResourceApplied { kind:event.kind.clone(), resource: event.object }
                            }
                            "BatchProcess" => {
                                  KubeMessage::ResourceBatch { kind:event.kind.clone(), resources: event.object } 
                            }
                            _ => todo!()
                        };

                        consumer.send_message(message).unwrap();
                    }
                    None => todo!()
                }
            }
            Err(e) => todo!()
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
        myself: ActorRef<Self::Msg>,
        args: (),
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
