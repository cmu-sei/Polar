use kube::api::ObjectList;
use polar::DispatcherMessage;
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use rkyv::rancor::Error;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Debug;
use tracing::{error, info, warn};

// TODO: Update names to follow a better format - kubernetes.<CLUSTER_NAME>.<ROLE>.<RESOURCE>
// clustername should come from configuration in the future
// ideally we could name consumers and observers exactly after the resource they're supposed to look for, but the strings aren't known yet
// It looks like the API sets types to things like PodList, for example, if this is open information, we can go off of the latest api!
// Also, make sure we have names for each reasource type, KUBE_POD_OBSERVER, KUBE_NODE_OBSERVER etc
pub const KUBERNETES_OBSERVER: &str = "kubernetes.cluster.observer.pods";
pub const KUBERNETES_CONSUMER: &str = "kubernetes.cluster.consumer.Pod";
pub const BATCH_PROCESS_ACTION: &str = "BatchProcess";
pub const RESOURCE_APPLIED_ACTION: &str = "Applied";
pub const RESOURCE_DELETED_ACTION: &str = "Delete";
pub const RESYNC_ACTIUON: &str = "Resync";

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
    ResourceBatch { kind: String, resources: Value },
    /// A new or updated resource has been observed
    ///
    ResourceApplied { kind: String, resource: Value },

    /// A resource has been deleted
    ResourceDeleted { kind: String, resource: Value },

    /// Indicates that the watcher is resetting and will re-list resources
    ResyncStarted { kind: String, resource: Value },
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

pub fn get_consumer_name(cluster_name: &str, kind: &str) -> String {
    format!("kubernetes.{cluster_name}.consumer.{kind}")
}
pub fn get_observer_name(cluster_name: &str, kind: &str) -> String {
    format!("kubernetes.{cluster_name}.observer.{kind}")
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

    pub fn deserialize_and_dispatch(message: Vec<u8>, cluster: &str) {
        // 1) Parse the raw message into your RawKubeEvent
        let raw_event: RawKubeEvent = match serde_json::from_slice(&message) {
            Ok(ev) => ev,
            Err(err) => {
                error!("Failed to deserialize RawKubeEvent: {}", err);
                return;
            }
        };

        let kind = raw_event.kind; // e.g. "Pod"
        let action = raw_event.action; // e.g. "Applied" or "BatchProcess"
        let payload = raw_event.object; // serde_json::Value

        // 2) Build the consumer’s name from cluster + kind
        let consumer_name = get_consumer_name("cluster", &kind);

        // 3) Look up the actor in Ractor’s registry
        match where_is(consumer_name.clone()) {
            Some(consumer_ref) => {
                // 4) Build a typed KubeMessage based on action
                let kube_msg = match action.as_str() {
                    BATCH_PROCESS_ACTION => KubeMessage::ResourceBatch {
                        kind: kind.clone(),
                        resources: payload,
                    },
                    RESOURCE_APPLIED_ACTION => KubeMessage::ResourceApplied {
                        kind: kind.clone(),
                        resource: payload,
                    },
                    RESOURCE_DELETED_ACTION => KubeMessage::ResourceDeleted {
                        kind: kind.clone(),
                        resource: payload,
                    },
                    RESYNC_ACTIUON => KubeMessage::ResyncStarted {
                        kind: kind.clone(),
                        resource: payload,
                    },
                    other => {
                        warn!(
                            "Unhandled K8s action \"{}\" for kind \"{}\", dropping",
                            other, kind
                        );
                        return;
                    }
                };

                // 5) Send message, logging any failure
                if let Err(err) = consumer_ref.send_message(kube_msg) {
                    error!(
                        "Failed to send KubeMessage to {}: {}",
                        consumer_ref.get_name().unwrap_or_default(),
                        err
                    );
                }
            }

            None => {
                // No consumer registered for this kind
                warn!(
                    "No consumer found for topic \"{}\" (kind=\"{}\")",
                    consumer_name, kind
                );
            }
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
                MessageDispatcher::deserialize_and_dispatch(message, &topic);
            }
        }
        Ok(())
    }
}
