use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Debug;
pub mod flux;

pub const KUBERNETES_OBSERVER: &str = "kubernetes.cluster.observer.pods";

/// Deprecated: bypassed by every actual producer/consumer in this crate in
/// favor of a single topic multiplexed by kind. Superseded by
/// `kube_events_topic`, which additionally scopes by cluster (issue #236).
/// Remove once lib.rs and supervisor.rs are migrated off it.
#[deprecated(note = "use kube_events_topic(cluster_uid) instead")]
pub const KUBERNETES_CONSUMER: &str = "kubernetes.cluster.consumer.Pod";

pub const BATCH_PROCESS_ACTION: &str = "BatchProcess";
pub const RESOURCE_APPLIED_ACTION: &str = "Applied";
pub const RESOURCE_DELETED_ACTION: &str = "Delete";
pub const RESYNC_ACTIUON: &str = "Resync"; // pre-existing typo, left alone -- not touching public API names as a drive-by
pub const KIND_OCI_REPOSITORY: &str = "OCIRepository";
pub const KIND_KUSTOMIZATION: &str = "Kustomization";

/// Fixed, well-known discovery topics (issue #236). Unlike the per-cluster
/// events topic below, both sides need to agree on these at compile time.
pub const KUBERNETES_DISCOVERY_ANNOUNCE: &str = "polar.kubernetes.discovery.announce";
pub const KUBERNETES_DISCOVERY_QUERY: &str = "polar.kubernetes.discovery.query";

/// The per-cluster events topic a kube-observer publishes RawKubeEvents to,
/// and a kube-consumer dynamically subscribes to after learning the
/// cluster's UID via KUBERNETES_DISCOVERY_ANNOUNCE. Centralized here so the
/// observer and consumer crates can't drift on the format string.
pub fn kube_events_topic(cluster_uid: &str) -> String {
    format!("polar.kubernetes.{cluster_uid}.events")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RawKubeEvent {
    /// e.g. "Pod", "ConfigMap", "Service"
    pub kind: String,
    /// e.g. "Applied", "Deleted", "InitApply"
    pub action: String,
    /// the raw JSON of the object
    pub object: Value,
    pub resource_version: Option<String>,
    /// UID of the kube-system namespace in the cluster this event was
    /// observed in. Stamped by the observer at emit time. See issue #236.
    pub cluster_uid: String,
}

/// Messages intended to be serialized and sent across the broker boundary to consumers for processing
/// Generics make things 10x simpler here, as all types from the k8s_openapi crate can be serialized with serde.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KubeMessage {
    ResourceBatch { kind: String, resources: Value },
    ResourceApplied { kind: String, resource: Value },
    ResourceDeleted { kind: String, resource: Value },
    ResyncStarted { kind: String, resource: Value },
}
