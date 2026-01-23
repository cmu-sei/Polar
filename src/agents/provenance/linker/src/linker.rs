use cyclonedx_bom::prelude::*;
use neo4rs::Graph;
use neo4rs::Query;
use ractor::async_trait;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use tracing::{debug, info, warn};

use crate::ArtifactType;

/// An actor responsible for connecting distinct knowledge graph domains.
/// Potential planes include build, runtime, and artifact, where semantic continuity isn't always possible.
pub struct ProvenanceLinker;

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

pub struct ProvenanceLinkerState {
    graph: Graph,
}

pub struct ProvenanceLinkerArgs {
    pub graph: Graph,
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
        debug!("{myself:?} starting!");
        Ok(ProvenanceLinkerState { graph: args.graph })
    }

    async fn post_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            LinkerCommand::LinkArtifacts {
                artifact_id,
                artifact_type,
                related_names,
                version,
            } => {
                tracing::trace!("Recevied a LinkArtifacts directive");
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
                uri,
                digest,
                media_type,
            } => {
                tracing::trace!("Recevied a LinkContainerImages directive");
                // Invariant: “Every observed container image in the system has a canonical reference node.”
                info!("Updating reference to container: {uri} with digest: {digest}");
                let query = format!(
                    r#"
                    MERGE (ref:ContainerImageReference {{
                      normalized: '{uri}'
                    }})
                    SET
                      ref.digest = '{digest}',
                      ref.media_type = '{media_type}',
                      ref.last_resolved = timestamp()

                    WITH ref
                    MATCH (tag:ContainerImageTag)
                    WHERE tag.location = ref.normalized
                    MERGE (tag)-[:IDENTIFIES]->(ref)
                    "#
                );
                tracing::debug!(query);

                state.graph.run(Query::new(query.to_string())).await?;
            }
            // TODO: handle other commands as they arise
            _ => warn!("Received unexpected message"),
        }
        Ok(())
    }
}
