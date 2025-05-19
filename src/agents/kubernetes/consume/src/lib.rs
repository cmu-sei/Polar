use cassini::{client::TcpClientMessage, ClientMessage};
use neo4rs::{Config, Graph};
use ractor::registry::where_is;
use serde::{Serialize, Deserialize};
use std::{error::Error, fmt::Debug};

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

