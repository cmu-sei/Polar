use neo4rs::BoltType;
use polar::graph::{GraphControllerMsg, GraphNodeKey};
use polar::{impl_graph_controller, NormalizedSbom};
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
            Self::OCIConfig { digest, ..} => (
                format!("(:OCIConfig {{ digest: ${prefix}_digest }})"),
                vec![(format!("{prefix}_digest"), digest.clone().into())],
            ),
            ArtifactNodeKey::Sbom {
                uid,
                sbom
            } =>
            (
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
                            BoltType::String(format!("{}", sbom.format.as_str()).into()),
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

impl_graph_controller!(LinkerGraphController, node_key = ArtifactNodeKey);
