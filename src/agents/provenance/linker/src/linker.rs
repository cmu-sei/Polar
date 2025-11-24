use cyclonedx_bom::prelude::*;
use neo4rs::Graph;
use neo4rs::Query;
use polar::get_neo_config;
use polar::ProvenanceEvent;
use ractor::async_trait;
use ractor::concurrency::Duration;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use reqwest::Client;
use rkyv::rancor;
use tokio::task::AbortHandle;
use tracing::{debug, error, info, warn};

use crate::ArtifactType;

/// An actor responsible for connecting distinct knowledge graph domains.
/// Potential planes include build, runtime, and artifact, where semantic continuity isn't always possible.
pub struct ProvenanceLinker;

impl ProvenanceLinker {
    pub fn handle_message(payload: Vec<u8>) -> Option<LinkerCommand> {
        if let Ok(event) = rkyv::from_bytes::<ProvenanceEvent, rancor::Error>(&payload) {
            match event {
                ProvenanceEvent::ImageRefResolved {
                    id,
                    digest,
                    media_type,
                } => Some(LinkerCommand::LinkContainerImages {
                    id,
                    digest,
                    media_type,
                }),
                _ => todo!(),
            }
        } else {
            warn!("Failed to deserialize message into provenance event.");
            None
        }
    }
}
pub enum LinkerCommand {
    LinkContainerImages {
        id: String,
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

pub struct ProvenanceLinkerState {
    graph: Graph,
    interval: Duration,
    abort_handle: Option<AbortHandle>,
}

pub struct ProvenanceLinkerArgs {
    pub graph: Graph,
    pub interval: Duration,
}

impl ProvenanceLinker {
    async fn link_sboms(
        graph: &Graph,
        artifact_id: String,
        artifact_type: ArtifactType,
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
        // TODO: this might
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
        tracing::info!("{myself:?} starting...");

        Ok(ProvenanceLinkerState {
            graph: args.graph,
            interval: args.interval,
            abort_handle: None,
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::info!("Counting actor handle message...");

        match message {
            LinkerCommand::LinkArtifacts {
                artifact_id,
                artifact_type,
                related_names,
                version,
            } => {
                ProvenanceLinker::link_sboms(
                    &state.graph,
                    artifact_id,
                    artifact_type,
                    related_names,
                    version,
                )
                .await
                .expect("Expected to link sboms");
            }
            //TODO: Add another handler for linking package files in gtlab to container images deployed in k8s and their sboms
            LinkerCommand::LinkContainerImages {
                id,
                digest,
                media_type,
            } => {
                todo!()
                // Invariant: “Every observed container image in the system has a canonical reference node.”
                // let query = "
                //     MATCH (ref:ContainerImageReference)
                //     WITH ref
                //     MATCH (tag:ContainerImageTag)
                //     WITH tag
                //     WHERE ref.normalized = tag.location
                //     MERGE (ref)<-[:IDENTIFIES]-(tag)
                //     ";

                // tracing::debug!(query);

                // if let Err(e) = state.graph.run(Query::new(query.to_string())).await {
                //     tracing::warn!("{e}");
                //     myself
                //         .send_message(ProvenanceLinkerMessage::Backoff(Some(e.to_string())))
                //         .expect("Expected to forward message to self");
                // }
            }

            _ => todo!("Handle other commands"),
        }
        Ok(())
    }
}
