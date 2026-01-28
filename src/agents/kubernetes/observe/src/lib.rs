pub mod pods;
pub mod supervisor;

use cassini_client::TcpClientMessage;
use ractor::registry::where_is;
use tracing::warn;

pub const TCP_CLIENT_NAME: &str = "kubernetes.cluster_name.supervisor_name.client";

/// Helper struct to map container images to registries + meta
/// TODO: unfcomment if we need this later
// struct ContainerImageRef {
//     registry: String,
//     project_path: String,
//     tag: Option<String>,
//     digest: Option<String>,
// }

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
    Secrets,
}

pub struct KubernetesObserverArgs {
    pub namespace: String,
}
