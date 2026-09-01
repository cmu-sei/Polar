//! Kubernetes consumer: entry point and shared crate wiring.
//!
//! Resource projection logic -- the `GraphOperable` trait and its
//! per-resource impls -- lives in `projections/`; see that module's doc
//! comment for the full projection philosophy, timestamp conventions, and
//! image-attestation policy. This file is deliberately thin: module
//! declarations, the supervisor actor's state types, and nothing else.

use polar::cassini::TcpClient;
use polar::graph::controller::{GraphController, GraphOp};
use ractor::ActorRef;

pub mod projections;
pub mod supervisor;

pub use projections::GraphOperable;

pub const BROKER_CLIENT_NAME: &str = "kubernetes.cluster.cassini.client";

// ---------------------------------------------------------------------------
// Actor state
// ---------------------------------------------------------------------------

pub struct KubeConsumerState {
    pub graph_controller: ActorRef<GraphOp>,
    pub broker_client: TcpClient,
}

pub struct KubeConsumerArgs {
    pub graph_controller: ActorRef<GraphOp>,
    pub broker_client: TcpClient,
}

pub struct ResourceConsumerState {
    pub graph_controller: GraphController,
    pub kind: &'static str,
}
