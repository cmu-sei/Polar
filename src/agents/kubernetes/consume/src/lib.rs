use cassini_client::TcpClientMessage;
use neo4rs::{Config, Graph};
use ractor::ActorRef;

pub mod pods;
pub mod supervisor;

pub const BROKER_CLIENT_NAME: &str = "kubernetes.cluster.cassini.client";

pub struct KubeConsumerState {
    pub registration_id: String,
    graph: Graph,
    pub broker_client: ActorRef<TcpClientMessage>,
}

pub struct KubeConsumerArgs {
    pub registration_id: String,
    pub graph_config: Config,
    pub broker_client: ActorRef<TcpClientMessage>,
}
