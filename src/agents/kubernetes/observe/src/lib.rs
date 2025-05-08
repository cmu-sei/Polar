// Now assume you have another Data Agent scraping GitLab. It should collect:

//     Project registry paths (e.g., registry.gitlab.com/my-org/my-app)

//     CI/CD pipeline metadata

//     Known tags/digests

//     Commit SHA metadata, if available

// This allows you to normalize both sides (pod images + GitLab data) into a shared format like:
pub mod supervisor;

pub mod pods;

use cassini::{client::TcpClientMessage, ClientMessage};
use kube::api::ObjectList;
use ractor::registry::where_is;
use serde::{Deserialize, Serialize};
use tracing::warn;


// TODO: Update names to follow a better format - kubernetes.<CLUSTER_NAME>.<ROLE>.<RESOURCE>
pub const KUBERNETES_OBSERVER: &str = "kubernetes:default:observer:pods";
pub const KUBERNETES_CONSUMER: &str = "kubernetes:minikube:consumer:pods";

pub const TCP_CLIENT_NAME: &str = "kubernetes.cluster_name.supervisor_name.client";

/// Helper struct to map container images to registries + meta
struct ContainerImageRef {
    registry: String,
    project_path: String,
    tag: Option<String>,
    digest: Option<String>,
}

pub struct KubernetesObserver;

pub struct KubernetesObserverState {
    pub namespace: String,
    pub client: kube::Client,
}

/// Standard messages exchanged by the observer actors internal to the agent
pub enum KubernetesObserverMessage {
    Pods,
    Deployments,
    ConfigMaps,
    Secrets
}

/// Messages intended to be serialized and set across the broker boundary to consumers for processing
/// Generics make things 10x simpler here, as all types from the k8s_openapi crate can be serialized with serde.
/// That said, it looks like they don't support rkyv serialization (yet) so there's a performance bottleneck.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KubeMessage<K: std::clone::Clone> {
    /// Observer read a batch of resources during initial sync with the server.
    ResourceBatch {
        resources: ObjectList<K>,
    },
    /// A new or updated resource has been observed
    /// 
    ResourceApplied {
        resource: K,
    },

    /// A resource has been deleted
    ResourceDeleted {
        resource: K,
    },

    /// Indicates that the watcher is resetting and will re-list resources
    ResyncStarted {
        namespace: Option<String>,
        resource_kind: String,
    },

    /// A resource has been observed during re-sync (initial apply)
    ResyncResource {
        resource: K,
    },

    /// Resync has completed and all InitApply resources have been listed
    ResyncCompleted {
        namespace: Option<String>,
        resource_kind: String,
    },
}

pub struct KubernetesObserverArgs {
    pub namespace: String,
}

pub fn send_to_client(registration_id: String, topic: String, payload: Vec<u8>) {
    let envelope = TcpClientMessage::Send(ClientMessage::PublishRequest {
        topic, payload: payload , registration_id: Some(registration_id)
    }
    );
    
    // send data for batch processing

    match where_is(TCP_CLIENT_NAME.to_owned()) {
        Some(client) => {
            if let Err(e) = client.send_message(envelope) {
                warn!("Failed to send message to client {e}");
            }
        },
        None => todo!("If no client present, drop the message?"),
    }
}