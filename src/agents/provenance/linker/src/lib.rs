use std::fmt::Debug;

use neo4rs::{BoltType, Graph};
use polar::graph::compile_graph_op;
use polar::graph::{GraphControllerMsg, GraphControllerState, GraphNodeKey, GraphOp};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, trace, warn};

use serde::{Deserialize, Serialize};
pub mod linker;
pub mod supervisor;
pub const PROVENANCE_LIKER_NAME: &str = "polar.provenance.linker";
pub const PROVENANCE_SUPERVISOR_NAME: &str = "polar.provenance.supervisor";
pub const BROKER_CLIENT_NAME: &str = "provenance.linker.tcp";
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArtifactType {
    ContainerImage,
    Sbom,
}

/// Minimal typed intent that processors emit.
/// CAUTION: Keep this small and stable; extend later with versions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ArtifactNodeKey {
    /// Our understanding of what an "artifact" in the general sense is.
    Artifact,
    PodContainer {
        pod_uid: String,
        container_name: String,
    },
    OCIRegistry {
        hostname: String,
    },
    /// A human-facing ref (tag or digest), not identity.
    ContainerImageRef {
        normalized: String,
    },
    /// Canonical OCI artifact (manifest / index)
    OCIArtifact {
        digest: String, // sha256:...
    },
    /// Content-addressed filesystem layer
    OCILayer {
        digest: String, // sha256:...
    },
    /// OCI config object
    OCIConfig {
        digest: String, // sha256:...
        os: String,
        arch: String,
        created: String,
        entrypoint: String,
        cmd: String,
    },
    /// Observed / asserted software component
    ComponentClaim {
        claim_type: String, // "rpm", "deb", "pip", "file", ...
        name: String,
        version: String,
    },
}

impl GraphNodeKey for ArtifactNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            Self::Artifact => (
                format!("(:Artifact)"), vec![]),
            Self::OCIRegistry { hostname } => (
                format!("(:OCIRegistry {{ hostname: ${prefix}_hostname }})"),
                vec![(format!("{prefix}_hostname"), hostname.clone().into())],
            ),
            Self::ContainerImageRef { normalized } => (
                format!("(:ContainerImage {{ normalized: ${prefix}_normalized }})"),
                vec![(format!("{prefix}_normalized"), normalized.clone().into())],
            ),

            Self::OCIArtifact { digest, .. } => (
                format!("(:OCIArtifact {{ digest: ${prefix}_digest }})"),
                vec![(format!("{prefix}_digest"), digest.clone().into())],
            ),
            Self::PodContainer { pod_uid, container_name } => (
                format!("(:PodContainer {{ container_name: ${prefix}_container_name, pod_uid: ${prefix}_pod_uid }})"),
            vec![
                (format!("{prefix}_container_name"), container_name.clone().into()),
                (format!("{prefix}_pod_uid"), pod_uid.clone().into()),
            ]),

            Self::OCILayer { digest } => (
                format!("(:OCILayer {{ digest: ${prefix}_digest }})"),
                vec![(format!("{prefix}_digest"), digest.clone().into())],
            ),

            // TODO: Add other types to the cypher
            Self::OCIConfig { digest, ..} => (
                format!("(:OCIConfig {{ digest: ${prefix}_digest }})"),
                vec![(format!("{prefix}_digest"), digest.clone().into())],
            ),

            Self::ComponentClaim {
                claim_type,
                name,
                version,
            } => (
                format!(
                    "(:ComponentClaim {{ type: ${prefix}_type, name: ${prefix}_name, version: ${prefix}_version }})"
                ),
                vec![
                    (format!("{prefix}_type"), claim_type.clone().into()),
                    (format!("{prefix}_name"), name.clone().into()),
                    (format!("{prefix}_version"), version.clone().into()),
                ],
            ),
        }
    }
}

/// A concrete instance of a GraphController for the artifact linker.

pub struct LinkerGraphController;

impl LinkerGraphController {
    /// Here we provide an async helper, just to make the Actor implementation as lean as possible. Async traits aren't well supported yet, so until then this is the best we've got
    async fn handle_op<ArtifactNodeKey>(
        graph: &Graph,
        op: &GraphOp<ArtifactNodeKey>,
    ) -> Result<(), ActorProcessingErr>
    where
        ArtifactNodeKey: GraphNodeKey + Debug,
    {
        let span = tracing::trace_span!("GraphController.handle_op");
        let _guard = span.enter();
        let q = compile_graph_op(&op);

        let mut txn = graph.start_txn().await?;
        debug!("{q:?}");
        txn.run(q)
            .await
            .map_err(|e| ActorProcessingErr::from(format!("neo4j execution failed: {:?}", e)))?;
        txn.commit().await?;
        trace!("transaction committed");
        Ok(())
    }
}

#[ractor::async_trait]
impl Actor for LinkerGraphController {
    type Msg = GraphControllerMsg<ArtifactNodeKey>;
    type State = GraphControllerState;
    type Arguments = Graph;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        graph: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting. Connecting to neo4j.");
        Ok(GraphControllerState { graph })
    }

    async fn handle(
        &self,
        _me: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            GraphControllerMsg::Op(op) => Self::handle_op(&state.graph, &op).await?,
        }
        Ok(())
    }
}
