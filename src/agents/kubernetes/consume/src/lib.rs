use neo4rs::{Config, Graph};

pub mod pods;
pub mod supervisor;

pub const BROKER_CLIENT_NAME: &str = "kubernetes.cluster.cassini.client";

pub struct KubeConsumerState {
    pub registration_id: String,
    graph: Graph,
}

pub struct KubeConsumerArgs {
    pub registration_id: String,
    pub graph_config: Config,
}
