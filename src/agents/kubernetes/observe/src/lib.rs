pub mod pods;
pub mod supervisor;

use cassini::{client::TcpClientMessage, ClientMessage};
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
/// Helper function to send a message to the cassini client actor
pub fn send_to_client(registration_id: String, topic: String, payload: Vec<u8>) {
    let envelope = TcpClientMessage::Send(ClientMessage::PublishRequest {
        topic,
        payload: payload,
        registration_id: Some(registration_id),
    });

    // send data for batch processing

    match where_is(TCP_CLIENT_NAME.to_owned()) {
        Some(client) => {
            if let Err(e) = client.send_message(envelope) {
                warn!("Failed to send message to client {e}");
            }
        }
        None => todo!("If no client present, drop the message?"),
    }
}
