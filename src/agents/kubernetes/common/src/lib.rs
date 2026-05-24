use polar::DispatcherMessage;
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait, registry::where_is};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Debug;
use tracing::{error, info, warn};

pub mod flux;
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
pub const KIND_OCI_REPOSITORY: &str = "OCIRepository";
pub const KIND_KUSTOMIZATION: &str = "Kustomization";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RawKubeEvent {
    /// e.g. "Pod", "ConfigMap", "Service"
    pub kind: String,
    /// e.g. "Applied", "Deleted", "InitApply"
    pub action: String,
    /// the raw JSON of the object
    pub object: Value,

    pub resource_version: Option<String>,
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
