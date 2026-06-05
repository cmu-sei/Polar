use polar::StageOutcome;
use serde::{Deserialize, Serialize};
// ── Origin ─────────────────────────────────────────────────────────────────────

/// Source system identity carried on every canonical event.
/// Used for audit and the SOURCED_FROM edge — never branched on by the processor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildOrigin {
    /// Lowercase stable identifier: "gitlab", "github_actions", "cyclops", "gitea"
    pub system: String,
    /// The native ID in the source system: pipeline ID, run ID, Cyclops UUID
    pub native_id: String,
    /// Job-level native ID where applicable — GitLab job ID, Actions step ID.
    /// None for systems where job granularity isn't meaningful at event time.
    pub native_job_id: Option<String>,
    /// Deep link back to the source for operator debugging
    pub native_url: Option<String>,
}

// ── Backend ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendIdentity {
    pub node_label: String,
    pub identity_props: Vec<(String, String)>,
}

// ── Shared artifact domain types ───────────────────────────────────────────────
//
// These types are defined here because orchestrator_core cannot depend on polar.
// They are structurally identical to the corresponding types in polar — if you
// change one, change the other. The into_provenance_event bridge in this crate
// maps between them.

/// A software package reference, used in SBOM graph fragments.
/// Purl is the stable identity; name and version are SET properties.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SbomComponent {
    pub purl: String,
    pub name: String,
    pub version: String,
    pub component_type: String,
}

/// A dependency edge from the CycloneDX `dependencies` array.
/// Encodes "from_ref depends on each of to_refs."
/// NOT derived from the flat component list — this is the actual tree
/// structure the SBOM generator recorded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SbomDependencyEdge {
    pub from_ref: String,
    pub to_refs: Vec<String>,
}

/// A single OCI image layer with its position in the stack.
/// Uses uncompressed diff ID as identity — this is what syft references
/// in layer attribution and what the image config records in rootfs.diff_ids.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OciLayer {
    pub order: u32,
    /// sha256 of the uncompressed layer tarball
    pub diff_id: String,
    /// Path within the tarball — for local introspection only, not identity
    pub tar_path: String,
}

// ── Envelope ───────────────────────────────────────────────────────────────────

/// Canonical event envelope. All CI integrations normalize to this shape
/// before publishing to the build events topic. The processor and the artifact
/// linker both subscribe to this topic and see only this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildEvent {
    /// Canonical UUID — assigned by the normalizer if the source system
    /// doesn't provide a stable job-level UUID
    pub build_id: String,
    /// Origin system metadata — for audit and SOURCED_FROM edge construction only.
    /// Never branched on by the processor or linker — if you need to branch, fix
    /// the normalizer.
    pub source: BuildOrigin,
    /// Unix seconds from the source system clock
    pub emitted_at: i64,
    /// Unix seconds stamped by the consumer at processing time
    pub observed_at: i64,
    pub payload: BuildEventPayload,
}

// ── Payload ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BuildEventPayload {
    // ── Build execution lifecycle ──────────────────────────────────────────────
    // Consumed by the build processor. Ignored by the artifact linker.
    ExecutionStarted {
        commit_sha: String,
        ref_name: String,
        repo_url: String,
        /// None when not reliably available — honest null beats fabricated identity
        triggered_by: Option<String>,
        backend: Option<BackendIdentity>,
    },
    StageStarted {
        stage_name: String,
        /// Scoped to the build — normalizer must ensure (build_id, stage_id) uniqueness
        stage_id: String,
    },
    StageCompleted {
        stage_name: String,
        stage_id: String,
        duration_secs: u64,
        outcome: StageOutcome,
    },
    ExecutionCompleted {
        duration_secs: u64,
    },
    ExecutionFailed {
        reason: String,
        /// Which stage failed, if attributable — free string, not an enum,
        /// because external systems won't map to internal failure vocabulary
        stage: Option<String>,
    },
    ExecutionCancelled {
        reason: Option<String>,
    },
    VulnerabilityFound {
        severity: String,
        identifier: String,
        /// Content hash of the artifact the vuln was found in, if known at emit time
        in_artifact: Option<String>,
    },

    // ── Artifact domain ────────────────────────────────────────────────────────
    // Consumed by the artifact linker. Ignored by the build processor.
    /// A raw pipeline artifact was produced: SBOM file, ELF binary, test report,
    /// scan result, OCI manifest bundle, etc. Keyed on content_hash (sha256 of
    /// the file bytes). Not for OCI container images — use ContainerImageCreated.
    ArtifactProduced {
        /// sha256:<hex> computed by content-hash-file in the pipeline
        content_hash: String,
        /// Free string: "sbom", "elf-binary", "test-report", "scan-report",
        /// "oci-manifest-bundle", etc.
        artifact_type: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        content_type: Option<String>,
    },

    /// An SBOM was parsed and its dependency graph extracted.
    /// Carries the full graph fragment so the linker can write Package nodes
    /// and DEPENDS_ON edges in one pass without re-fetching the file.
    SbomAnalyzed {
        filename: String,
        /// Join key to the ArtifactProduced event for this SBOM file
        artifact_content_hash: String,
        root: Option<SbomComponent>,
        components: Vec<SbomComponent>,
        edges: Vec<SbomDependencyEdge>,
    },

    /// A compiled binary was linked to its source package and SBOM.
    /// binding_digest = sha256(binary:cargo_toml:cargo_lock:source_tree)
    /// Recorded for audit — not used for graph structure.
    BinaryLinked {
        binary_content_hash: String,
        binary_name: String,
        /// Purl of the package this binary was compiled from.
        /// Join key to the Package node created by SbomAnalyzed.
        root_purl: String,
        /// Join key to the Sbom node created by SbomAnalyzed.
        sbom_content_hash: String,
        #[serde(default)]
        binding_digest: Option<String>,
    },

    /// An OCI container image was built and is available as a local tarball.
    /// Emitted before registry push. config_digest is the stable content
    /// identity across registries. The manifest digest (post-push) arrives
    /// later via ArtifactProduced with artifact_type "oci-image".
    ///
    /// Non-image OCI artifacts (manifest bundles, Helm charts, Flux packages)
    /// use ArtifactProduced, not this variant.
    ContainerImageCreated {
        image_name: String,
        /// content_hash of the tarball on disk — identity before registry push
        tarball_hash: String,
        /// Digest of the OCI config blob — stable across re-uploads
        config_digest: String,
        layers: Vec<OciLayer>,
        #[serde(default)]
        os: String,
        #[serde(default)]
        arch: String,
        #[serde(default)]
        created: String,
        #[serde(default)]
        entrypoint: String,
        #[serde(default)]
        cmd: String,
        #[serde(default)]
        repo_tags: Vec<String>,
    },
}
