use cyclonedx_bom::prelude::*;

use neo4rs::Graph;
use neo4rs::Query;
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

impl ProvenanceLinker {
    async fn link_sboms(
        graph: &Graph,
        artifact_id: String,
        _artifact_type: ArtifactType,
        related_names: Vec<NormalizedString>,
        version: NormalizedString,
    ) -> Result<(), ActorProcessingErr> {
        info!("Linking SBOM artifact {}", artifact_id);

        // Example: link SBOM components to GitlabPackages or ContainerImageTags
        // (depending on what exists in your ontology)
        for name in related_names {
            let cypher = format!(
                r#"
                MATCH (a:Artifact {{ id: '{id}' }})
                MERGE (s:SoftwareComponent {{ name: '{name}' }})
                MERGE (a)-[:DESCRIBES_COMPONENT]->(s)
                WITH s
                OPTIONAL MATCH (pkg:GitlabPackage {{ name: s.name }})
                MERGE (s)-[:IDENTIFIES_PACKAGE]->(pkg)
                "#,
                id = artifact_id,
                name = name
            );

            debug!(%cypher, "Linking SBOM component");
            if let Err(e) = graph.run(Query::new(cypher)).await {
                warn!("Failed to link component {}: {:?}", name, e);
            }
        }

        // Optionally, if version info is present, link to container tags or package versions
        if &version.to_string() != "None" {
            let cypher = format!(
                r#"
                MATCH (a:Artifact {{ id: '{id}' }})
                MATCH (pkg:GitlabPackage {{ version: '{ver}' }})
                MERGE (a)-[:DESCRIBES_VERSION]->(pkg)
            "#,
                id = artifact_id,
                ver = version
            );

            debug!(%cypher, "Linking SBOM to versioned package");
            let _ = graph.run(Query::new(cypher)).await;
        }

        Ok(())
    }
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
        myself: ActorRef<Self::Msg>,
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
                    Property("digest".into(), serde_json::json!(digest)),
                    Property("media_type".into(), serde_json::json!(media_type)),
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
                        serde_json::json!(artifact_id.clone()),
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
