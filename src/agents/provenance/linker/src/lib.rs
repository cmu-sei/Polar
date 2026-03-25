use neo4rs::BoltType;
use polar::{NormalizedSbom, graph::controller::GraphNodeKey};
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
    Sbom {
        /// A Hash generated from the bytes of the sbom file.
        uid: String,
        sbom: NormalizedSbom,
    },
    /// Observed / asserted software component
    Component {
        claim_type: String, // "rpm", "deb", "pip", "file", ...
        name: String,
        version: String,
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
            ArtifactNodeKey::Sbom { uid, sbom } => (
                format!(
                    "({prefix}:SBOM {{ artifact_uid: ${prefix}_artifact_uid, format: ${prefix}_format, spec_version: ${prefix}_spec_version }})"
                ),
                vec![
                    (
                        format!("{prefix}_artifact_uid"),
                        BoltType::String(uid.clone().into()),
                    ),
                    (
                        format!("{prefix}_format"),
                        BoltType::String(sbom.format.as_str().to_string().into()),
                    ),
                    (
                        format!("{prefix}_spec_version"),
                        BoltType::String(sbom.spec_version.clone().into()),
                    ),
                ],
            ),
            Self::Component {
                claim_type,
                name,
                version,
            } => (
                format!(
                    "({prefix}:ComponentClaim {{ type: ${prefix}_type, name: ${prefix}_name, version: ${prefix}_version }})"
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
