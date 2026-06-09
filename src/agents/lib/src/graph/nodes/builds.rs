use crate::graph::controller::GraphNodeKey;
use neo4rs::BoltType;
use serde::Deserialize;
use serde::Serialize;

// ── Node key vocabulary ────────────────────────────────────────────────────────

/// Node key enum for the Cyclops provenance processor.
///
/// Only two node types are *owned* by this processor — `BuildJob` and
/// `BuildJobState`. All other node types (`GitCommit`, `Image`,
/// `PodContainer`) are owned by other agents. This processor only creates
/// edges to them — never upserts their properties — to avoid clobbering
/// data that the authoritative agent manages.
///
/// The `cypher_match` implementation generates parameterized MERGE clauses
/// for each node type, following the same prefix convention as `KubeNodeKey`
/// so that `compile_graph_op` can compose multi-node queries without
/// parameter name collisions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BuildNodeKey {
    /// The abstract type node for build executions.
    /// All BuildJob instances link to this via IS.
    /// Analogous to the Artifact type node in the artifact domain.
    BuildExecution,

    /// Cannonical state node
    State,
    /// A single Cyclops build execution. This is the linchpin node — it
    /// connects a git commit to the image that was built from it.
    /// Identified by the UUID assigned at build request time.
    BuildJob { build_id: String },

    /// An immutable state snapshot of a BuildJob at a point in time.
    /// Follows the same temporal state pattern as KubeNodeKey::PodState —
    /// one state node per transition, linked via TRANSITIONED_TO edges.
    BuildJobState {
        build_id: String,
        valid_from: String,
    },

    /// A job on a backend execution system.
    /// Self-describing — the label and merge properties come from
    /// JobGraphIdentity, so this variant works for any backend
    /// without the processor knowing which one it is.
    BackendJob {
        node_label: String,
        identity_props: Vec<(String, String)>,
    },
    /// A named stage within a BuildExecution. Keyed on (build_id, stage_id)
    /// because stage_id alone isn't guaranteed unique across backends.
    /// Stage-level timing is where bottleneck analysis lives.
    BuildStage { build_id: String, stage_id: String },

    /// An immutable, content-addressed output of a BuildExecution.
    /// Digest is the sole primary key — two builds producing the same digest
    /// correctly converge to the same node (reproducible build detection).
    /// The PRODUCED edge carries the relationship back to the BuildJob.
    BuildArtifact { content_hash: String },

    /// A vulnerability found during a build scan. Keyed on identifier so that
    /// the same CVE/GHSA found across multiple builds converges to one node —
    /// the FOUND_VULNERABILITY edge on BuildJob records which builds saw it,
    /// and FOUND_IN links it to the specific artifact if the scanner attributed it.
    Vulnerability { identifier: String },
}

impl GraphNodeKey for BuildNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            BuildNodeKey::State => ("(:State)".to_string(), vec![]),
            BuildNodeKey::BuildStage { build_id, stage_id } => {
                let bid_k = format!("{prefix}_build_id");
                let sid_k = format!("{prefix}_stage_id");
                (
                    format!("({prefix}:BuildStage {{ build_id: ${bid_k}, stage_id: ${sid_k} }})"),
                    vec![
                        (bid_k, BoltType::String(build_id.clone().into())),
                        (sid_k, BoltType::String(stage_id.clone().into())),
                    ],
                )
            }

            BuildNodeKey::BuildArtifact { content_hash } => {
                let ck = format!("{prefix}_content_hash");
                (
                    format!("({prefix}:BuildArtifact {{ content_hash: ${ck} }})"),
                    vec![(ck, BoltType::String(content_hash.clone().into()))],
                )
            }

            BuildNodeKey::Vulnerability { identifier } => {
                let ik = format!("{prefix}_identifier");
                (
                    format!("({prefix}:Vulnerability {{ identifier: ${ik} }})"),
                    vec![(ik, BoltType::String(identifier.clone().into()))],
                )
            }
            BuildNodeKey::BackendJob {
                node_label,
                identity_props,
            } => {
                let mut params = vec![];
                let prop_str = identity_props
                    .iter()
                    .map(|(k, v)| {
                        let param_key = format!("{prefix}_{k}");
                        params.push((param_key.clone(), BoltType::String(v.clone().into())));
                        format!("{k}: ${param_key}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");

                (format!("({prefix}:{node_label} {{ {prop_str} }})"), params)
            }
            BuildNodeKey::BuildExecution => (
                format!("({prefix}:BuildExecution {{type: \"BuildExecution\"}})"),
                vec![(
                    format!("{prefix}_type"),
                    BoltType::String("BuildExecution".into()),
                )],
            ),
            BuildNodeKey::BuildJob { build_id } => {
                let id_k = format!("{prefix}_build_id");
                (
                    format!("({prefix}:BuildJob {{ build_id: ${id_k} }})"),
                    vec![(id_k, BoltType::String(build_id.clone().into()))],
                )
            }

            BuildNodeKey::BuildJobState {
                build_id,
                valid_from,
            } => {
                let id_k = format!("{prefix}_build_id");
                let vf_k = format!("{prefix}_valid_from");
                (
                    format!(
                        "({prefix}:BuildJobState {{ build_id: ${id_k}, valid_from: ${vf_k} }})"
                    ),
                    vec![
                        (id_k, BoltType::String(build_id.clone().into())),
                        (vf_k, BoltType::String(valid_from.clone().into())),
                    ],
                )
            }
        }
    }
}

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

    /// A container image identified by its config digest.
    /// This is the content identity — stable across registries.
    ContainerImage {
        config_digest: String,
    },
}

impl GraphNodeKey for ArtifactNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            Self::Artifact => (
                format!("({prefix}:Artifact {{type: \"Artifact\"}})"),
                vec![(
                    format!("{prefix}_type"),
                    BoltType::String("Artifact".into()),
                )],
            ),
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
            Self::ContainerImage { config_digest } => (
                format!("({prefix}:ContainerImage {{ config_digest: ${prefix}_config_digest }})"),
                vec![(
                    format!("{prefix}_config_digest"),
                    BoltType::String(config_digest.clone().into()),
                )],
            ),
        }
    }
}
