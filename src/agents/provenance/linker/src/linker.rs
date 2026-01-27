use cyclonedx_bom::prelude::*;

use polar::graph::{GraphControllerMsg, GraphOp, NodeKey, Property};
use ractor::async_trait;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use tracing::{debug, info, warn};

use crate::ArtifactType;

pub struct ProvenanceLinker;

pub struct ProvenanceLinkerState {
    compiler: ActorRef<GraphControllerMsg>,
}
pub struct ProvenanceLinkerArgs {
    pub compiler: ActorRef<GraphControllerMsg>,
}

pub enum LinkerCommand {
    LinkContainerImages {
        uri: String,
        digest: String,
        media_type: String,
    },
    LinkPackages,
    LinkArtifacts {
        artifact_id: String,
        artifact_type: ArtifactType,
        related_names: Vec<NormalizedString>,
        version: NormalizedString,
    },
}
#[async_trait]
impl Actor for ProvenanceLinker {
    type Msg = LinkerCommand;
    type State = ProvenanceLinkerState;
    type Arguments = ProvenanceLinkerArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        // Expect the supervisor to pass the compiler ActorRef via args.graph or separate arg.
        // For this example, assume ProvenanceLinkerArgs contains compiler_ref.
        Ok(ProvenanceLinkerState {
            // store compiler ref elsewhere
            compiler: args.compiler,
        })
    }

    async fn handle(
        &self,
        _me: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            LinkerCommand::LinkContainerImages {
                uri,
                digest,
                media_type,
            } => {
                // Upsert the ContainerImageReference node
                let node_key = NodeKey::ContainerImageRef {
                    normalized: uri.clone(),
                };
                let props = vec![
                    Property("digest".into(), polar::graph::GraphValue::String(digest)),
                    Property(
                        "media_type".into(),
                        polar::graph::GraphValue::String(media_type),
                    ),
                ];
                let op = GraphOp::UpsertNode {
                    key: node_key.clone(),
                    props,
                };
                // send to compiler
                state
                    .compiler
                    .send_message(GraphControllerMsg::Op(op))
                    .map_err(|e| {
                        ActorProcessingErr::from(format!(
                            "failed to send GraphOp to compiler: {:?}",
                            e
                        ))
                    })?;

                // Then link tags (existing ContainerImageTag nodes) to the ref
                // Here we model linking as an edge from tag -> ref. We build an edge op
                let from = NodeKey::ContainerImageRef {
                    normalized: uri.clone(),
                }; // could be tag node in other flows
                   // For simplicity assume tag nodes are identified by normalized tag location.
                let to = node_key.clone();
                let edge = GraphOp::EnsureEdge {
                    from,
                    to,
                    rel_type: "IDENTIFIES".to_string(),
                    props: vec![],
                };
                state
                    .compiler
                    .send_message(GraphControllerMsg::Op(edge))
                    .map_err(|e| {
                        ActorProcessingErr::from(format!(
                            "failed to send edge op to compiler: {:?}",
                            e
                        ))
                    })?;
            }

            LinkerCommand::LinkArtifacts {
                artifact_id,
                artifact_type: _,
                related_names,
                ..
            } => {
                // Create Artifact node and link components -> package nodes
                let art_key = NodeKey::OCIRegistry {
                    hostname: artifact_id.clone(),
                }; // pick appropriate node type; replace if artifact node exists
                let create_art = GraphOp::UpsertNode {
                    key: art_key.clone(),
                    props: vec![Property(
                        "artifact_id".into(),
                        polar::graph::GraphValue::String(artifact_id),
                    )],
                };
                state
                    .compiler
                    .send_message(GraphControllerMsg::Op(create_art))
                    .map_err(|e| {
                        ActorProcessingErr::from(format!("failed to send artifact upsert: {:?}", e))
                    })?;

                // For each related name, create a SoftwareComponent (you may add node type later); for now, model it as ContainerImageRef or package.
                for name in related_names {
                    let comp_key = NodeKey::ContainerImageRef {
                        normalized: name.to_string(),
                    };
                    let up = GraphOp::UpsertNode {
                        key: comp_key.clone(),
                        props: vec![],
                    };
                    state
                        .compiler
                        .send_message(GraphControllerMsg::Op(up.clone()))
                        .map_err(|e| {
                            ActorProcessingErr::from(format!(
                                "failed to send component upsert: {:?}",
                                e
                            ))
                        })?;

                    // Link artifact -> component
                    let edge = GraphOp::EnsureEdge {
                        from: art_key.clone(),
                        to: comp_key.clone(),
                        rel_type: "DESCRIBES_COMPONENT".to_string(),
                        props: vec![],
                    };
                    state
                        .compiler
                        .send_message(GraphControllerMsg::Op(edge))
                        .map_err(|e| {
                            ActorProcessingErr::from(format!(
                                "failed to send artifact->component link: {:?}",
                                e
                            ))
                        })?;
                }
            }

            _ => warn!("unexpected linker command"),
        }
        Ok(())
    }
}
