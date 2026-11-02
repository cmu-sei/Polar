/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

use crate::graph::controller::{CypherIdentifierError, validate_cypher_identifier};
use crate::graph::controller::{GraphNodeKey, NodeIdentity, NodeKeyVocabulary};
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
    /// without the processor knowing which one it is. Wrapped in
    /// BackendJobIdentity rather than holding these fields directly --
    /// see its own doc comment; the fields are private specifically to
    /// force construction through its validating constructor.
    BackendJob(BackendJobIdentity),

    /// A named stage within a BuildExecution. Keyed on (build_id, stage_id)
    /// because stage_id alone isn't guaranteed unique across backends.
    /// Stage-level timing is where bottleneck analysis lives.
    BuildStage { build_id: String, stage_id: String },

    /// An immutable, content-addressed output of a BuildExecution.
    /// Digest is the sole primary key — two builds producing the same digest
    /// correctly converge to the same node (reproducible build detection).
    /// The PRODUCED edge carries the relationship back to the BuildJob.
    BuildArtifact { content_hash: String },
}

impl GraphNodeKey for BuildNodeKey {
    fn identity(&self) -> NodeIdentity {
        let s = |v: &String| BoltType::String(v.clone().into());
        match self {
            // The taxonomy anchor, bound like any other node (docs/architecture/graph-write-path.md).
            BuildNodeKey::State => NodeIdentity::fixed(&["State"], vec![]),
            BuildNodeKey::BuildStage { build_id, stage_id } => NodeIdentity::fixed(
                &["BuildStage"],
                vec![("build_id", s(build_id)), ("stage_id", s(stage_id))],
            ),
            BuildNodeKey::BuildArtifact { content_hash } => {
                NodeIdentity::fixed(&["BuildArtifact"], vec![("content_hash", s(content_hash))])
            }
            // Validated by BackendJobIdentity::new; known only at run time.
            BuildNodeKey::BackendJob(identity) => NodeIdentity::runtime(
                vec![identity.node_label.clone()],
                identity
                    .identity_props
                    .iter()
                    .map(|(k, v)| (k.clone(), s(v)))
                    .collect(),
            ),
            BuildNodeKey::BuildExecution => NodeIdentity::fixed(
                &["BuildExecution"],
                vec![("type", BoltType::String("BuildExecution".into()))],
            ),
            BuildNodeKey::BuildJob { build_id } => {
                NodeIdentity::fixed(&["BuildJob"], vec![("build_id", s(build_id))])
            }
            BuildNodeKey::BuildJobState {
                build_id,
                valid_from,
            } => NodeIdentity::fixed(
                &["BuildJobState"],
                vec![("build_id", s(build_id)), ("valid_from", s(valid_from))],
            ),
        }
    }
}

impl NodeKeyVocabulary for BuildNodeKey {
    fn every_variant() -> Vec<Self> {
        let s = || "x".to_string();
        let all = vec![
            BuildNodeKey::BuildExecution,
            BuildNodeKey::State,
            BuildNodeKey::BuildJob { build_id: s() },
            BuildNodeKey::BuildJobState {
                build_id: s(),
                valid_from: s(),
            },
            BuildNodeKey::BackendJob(
                BackendJobIdentity::new("GitlabRunnerJob", vec![("job_id".into(), s())])
                    .expect("a valid label and property"),
            ),
            BuildNodeKey::BuildStage {
                build_id: s(),
                stage_id: s(),
            },
            BuildNodeKey::BuildArtifact { content_hash: s() },
        ];
        // A variant added to the enum does not compile until it is named here, beside the list it must join.
        for key in &all {
            match key {
                BuildNodeKey::BuildExecution
                | BuildNodeKey::State
                | BuildNodeKey::BuildJob { .. }
                | BuildNodeKey::BuildJobState { .. }
                | BuildNodeKey::BackendJob(_)
                | BuildNodeKey::BuildStage { .. }
                | BuildNodeKey::BuildArtifact { .. } => {}
            }
        }
        all
    }
}

/// A backend job's identity, self-describing via a caller-supplied
/// label and set of identity properties -- unlike this file's other
/// node key variants, whose labels/property names are fixed Rust
/// literals, a backend job's shape depends on which backend system
/// produced it (GitLab, Jenkins, etc.), known only at runtime, parsed
/// from that system's own metadata. node_label and every
/// identity_props key are validated at construction -- the two
/// positions Cypher can never parameterize -- the same pattern
/// DynamicNodeKey uses elsewhere in this crate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BackendJobIdentity {
    node_label: String,
    identity_props: Vec<(String, String)>,
}

impl BackendJobIdentity {
    pub fn new(
        node_label: impl Into<String>,
        identity_props: Vec<(String, String)>,
    ) -> Result<Self, CypherIdentifierError> {
        let node_label = node_label.into();
        validate_cypher_identifier(&node_label)?;
        for (k, _) in &identity_props {
            validate_cypher_identifier(k)?;
        }
        Ok(Self {
            node_label,
            identity_props,
        })
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

    // Representation of what a "fidning" is in the general sense as it relates to build artifacts.
    // Should only have incoming IS relationships
    Finding,
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
    SecurityAdvisory {
        identifier: String,
    },

    PackageStatus {
        identifier: String,
    },
}

impl GraphNodeKey for ArtifactNodeKey {
    fn identity(&self) -> NodeIdentity {
        let s = |v: &String| BoltType::String(v.clone().into());
        match self {
            Self::Artifact => NodeIdentity::fixed(
                &["Artifact"],
                vec![("type", BoltType::String("Artifact".into()))],
            ),
            Self::Finding => NodeIdentity::fixed(
                &["Finding"],
                vec![("type", BoltType::String("Finding".into()))],
            ),
            Self::OCIRegistry { hostname } => {
                NodeIdentity::fixed(&["OCIRegistry"], vec![("hostname", s(hostname))])
            }
            Self::ContainerImageRef { normalized } => {
                NodeIdentity::fixed(&["ContainerImage"], vec![("normalized", s(normalized))])
            }
            Self::OCIArtifact { digest, .. } => {
                NodeIdentity::fixed(&["OCIArtifact"], vec![("digest", s(digest))])
            }
            Self::OCILayer { digest } => {
                NodeIdentity::fixed(&["OCILayer"], vec![("digest", s(digest))])
            }
            Self::OCIConfig { digest, .. } => {
                NodeIdentity::fixed(&["OCIConfig"], vec![("digest", s(digest))])
            }
            Self::Binary { content_hash } => {
                NodeIdentity::fixed(&["Binary"], vec![("content_hash", s(content_hash))])
            }
            Self::Sbom {
                artifact_content_hash,
            } => NodeIdentity::fixed(
                &["Sbom"],
                vec![("artifact_content_hash", s(artifact_content_hash))],
            ),
            Self::Package { purl } => NodeIdentity::fixed(&["Package"], vec![("purl", s(purl))]),
            Self::BuildArtifact { content_hash } => {
                NodeIdentity::fixed(&["BuildArtifact"], vec![("content_hash", s(content_hash))])
            }
            Self::ContainerImage { config_digest } => NodeIdentity::fixed(
                &["ContainerImage"],
                vec![("config_digest", s(config_digest))],
            ),
            Self::SecurityAdvisory { identifier } => {
                NodeIdentity::fixed(&["SecurityAdvisory"], vec![("identifier", s(identifier))])
            }
            Self::PackageStatus { identifier } => {
                NodeIdentity::fixed(&["PackageStatus"], vec![("identifier", s(identifier))])
            }
        }
    }
}

impl NodeKeyVocabulary for ArtifactNodeKey {
    fn every_variant() -> Vec<Self> {
        let s = || "x".to_string();
        let all = vec![
            Self::Artifact,
            Self::Finding,
            Self::OCIRegistry { hostname: s() },
            Self::ContainerImageRef { normalized: s() },
            Self::OCIArtifact { digest: s() },
            Self::OCILayer { digest: s() },
            Self::OCIConfig {
                digest: s(),
                os: s(),
                arch: s(),
                created: s(),
                entrypoint: s(),
                cmd: s(),
            },
            Self::Binary { content_hash: s() },
            Self::Sbom {
                artifact_content_hash: s(),
            },
            Self::Package { purl: s() },
            Self::BuildArtifact { content_hash: s() },
            Self::ContainerImage { config_digest: s() },
            Self::SecurityAdvisory { identifier: s() },
            Self::PackageStatus { identifier: s() },
        ];
        // A variant added to the enum does not compile until it is named here, beside the list it must join.
        for key in &all {
            match key {
                Self::Artifact
                | Self::Finding
                | Self::OCIRegistry { .. }
                | Self::ContainerImageRef { .. }
                | Self::OCIArtifact { .. }
                | Self::OCILayer { .. }
                | Self::OCIConfig { .. }
                | Self::Binary { .. }
                | Self::Sbom { .. }
                | Self::Package { .. }
                | Self::BuildArtifact { .. }
                | Self::ContainerImage { .. }
                | Self::SecurityAdvisory { .. }
                | Self::PackageStatus { .. } => {}
            }
        }
        all
    }
}
