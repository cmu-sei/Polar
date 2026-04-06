use neo4rs::BoltType;
use polar::graph::controller::GraphNodeKey;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArtifactNodeKey {
    /// Our understanding of what an "artifact" in the general sense is.
    /// This particular nodekey represents the datatype, not an instance of an artifact.
    /// Other typed artifacts are related to it using an :IS
    Artifact,

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

    /// A compiled binary, keyed on its content hash.
    Binary {
        content_hash: String,
    },

    /// An SBOM document node. Keyed on the content hash of the file.
    /// This represents "we analyzed this specific SBOM file."
    Sbom {
        artifact_content_hash: String,
    },

    /// A software package identified by purl.
    /// Used for both the root package an SBOM describes and for each
    /// dependency in the tree. Purl is the merge key; name/version
    /// are SET properties.
    Package {
        purl: String,
    },

    /// A build artifact that was produced by a pipeline stage.
    /// Keyed on content hash. This is the provenance node — it
    /// links pipeline executions to their outputs.
    BuildArtifact {
        content_hash: String,
    },
}

impl GraphNodeKey for ArtifactNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            Self::Artifact => (format!("({prefix}:Artifact)"), vec![]),
            Self::OCIRegistry { hostname } => (
                format!("({prefix}:OCIRegistry {{ hostname: ${prefix}_hostname }})"),
                vec![(format!("{prefix}_hostname"), hostname.clone().into())],
            ),
            Self::ContainerImageRef { normalized } => (
                format!("({prefix}:ContainerImage {{ normalized: ${prefix}_normalized }})"),
                vec![(format!("{prefix}_normalized"), normalized.clone().into())],
            ),

            Self::OCIArtifact { digest, .. } => (
                format!("({prefix}:OCIArtifact {{ digest: ${prefix}_digest }})"),
                vec![(format!("{prefix}_digest"), digest.clone().into())],
            ),

            Self::OCILayer { digest } => (
                format!("({prefix}:OCILayer {{ digest: ${prefix}_digest }})"),
                vec![(format!("{prefix}_digest"), digest.clone().into())],
            ),
            Self::OCIConfig { digest, .. } => (
                format!("({prefix}:OCIConfig {{ digest: ${prefix}_digest }})"),
                vec![(format!("{prefix}_digest"), digest.clone().into())],
            ),
            Self::Binary { content_hash } => (
                format!("({prefix}:Binary {{ content_hash: ${prefix}_hash }})"),
                vec![(
                    format!("{prefix}_hash"),
                    BoltType::String(content_hash.clone().into()),
                )],
            ),
            Self::Sbom {
                artifact_content_hash,
            } => (
                format!("({prefix}:Sbom {{ artifact_content_hash: ${prefix}_hash }})"),
                vec![(
                    format!("{prefix}_hash"),
                    BoltType::String(artifact_content_hash.clone().into()),
                )],
            ),

            Self::Package { purl } => (
                format!("({prefix}:Package {{ purl: ${prefix}_purl }})"),
                vec![(
                    format!("{prefix}_purl"),
                    BoltType::String(purl.clone().into()),
                )],
            ),
            Self::BuildArtifact { content_hash } => (
                format!("({prefix}:BuildArtifact {{ content_hash: ${prefix}_hash }})"),
                vec![(
                    format!("{prefix}_hash"),
                    BoltType::String(content_hash.clone().into()),
                )],
            ),
        }
    }
}
