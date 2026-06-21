//! Provenance event types for the Polar system.
//!
//! This module defines the canonical event vocabulary used across all Polar agents.
//! Two wire formats are supported for the same logical type:
//!
//!   - **Rust agents** serialize [`ProvenanceEvent`] directly via rkyv and publish
//!     to the broker. Zero-copy, no allocation overhead on the deserialization path.
//!
//!   - **Nushell pipeline stages** cannot produce rkyv archives, so they emit JSON
//!     that matches [`ProvenanceEvent`] variant shapes directly. The supervisor
//!     deserializes these with `serde_json::from_slice::<ProvenanceEvent>`.
//!
//! Both paths converge at [`ProvenanceEvent`] before reaching any handler.
//! There is no intermediary wire type — `BuildEvent` and `BuildEventPayload` are retired.
//!
//! The `build_id` correlation field lives on the variants that need it
//! (specifically [`ProvenanceEvent::ExecutionStarted`] and related lifecycle events)
//! rather than on a shared envelope, because not all events originate from a CI pipeline.
//! A k8s observation has no `build_id`. Fabricating one would be dishonest.

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};

// ── Supporting types ───────────────────────────────────────────────────────────

/// Emitted by Polar when a new commit is detected on a tracked branch or ref.
///
/// This is the primary trigger for build jobs. The orchestrator consumes this
/// event off the Cassini broker, maps the repository URL to a pipeline image
/// via its `repo_mappings` config, and submits a build job for the given commit.
///
/// The event reflects what Polar *observed*, not what was directly reported by
/// a webhook. `observed_at` may lag the actual commit timestamp by the agent's
/// polling interval.
#[derive(Debug, Default, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub struct GitRepositoryUpdatedEvent {
    /// Unique identifier for this event. Used for idempotency — duplicate
    /// `event_id` values should be deduplicated rather than triggering two builds.
    pub event_id: String,
    /// HTTPS clone URL of the repository.
    pub http_url: Option<String>,
    /// SSH clone URL of the repository. Preferred when per-job SSH credentials
    /// are available.
    pub ssh_url: Option<String>,
    /// Full 40-character SHA of the commit that triggered this event.
    /// Never a branch name or short SHA.
    pub commit_sha: String,
    /// The ref on which this commit was observed (e.g. "refs/heads/main").
    pub git_ref: Option<String>,
    /// The repository's default branch name, if known.
    pub default_branch: Option<String>,
    /// Display name of the repository (e.g. "cmu-sei/Polar").
    pub repository_name: Option<String>,
    /// Identity of the commit author, if available.
    pub author: Option<String>,
    /// Unix timestamp when Polar's agent observed this commit.
    pub observed_at: i64,
}

/// Emitted when a git repository is first discovered by an observer agent.
#[derive(Debug, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub struct GitRepositoryDiscoveredEvent {
    pub http_url: Option<String>,
    pub ssh_url: Option<String>,
}

/// Identity of the execution backend that ran a build.
/// Self-describing — label and identity props vary by backend type.
/// Maps to `BuildNodeKey::BackendJob` in the graph.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct BackendIdentity {
    pub node_label: String,
    pub identity_props: Vec<(String, String)>,
}

/// Outcome of a completed build stage.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum StageOutcome {
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
}

/// A single OCI image layer with its position in the layer stack.
///
/// Uses the uncompressed diff ID as identity — this is what syft references
/// in layer attribution properties and what the image config records in
/// `rootfs.diff_ids`. The compressed digest (used by registries) is a
/// separate concept and lives on `OCILayer` nodes written by the resolver.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct OciLayerEntry {
    pub order: u32,
    /// sha256 of the uncompressed layer tarball
    pub diff_id: String,
    /// Path within the tarball — for local introspection only, not identity
    #[serde(default)]
    pub tar_path: String,
}

/// A software package reference, used in SBOM graph fragments.
///
/// Purl is the stable cross-build identity. Name and version are SET
/// properties on the resulting `Package` node — they can be updated
/// if a later SBOM provides better data. Purl is the MERGE key and
/// must never change for the same logical package.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct PackageRef {
    pub purl: String,
    pub name: String,
    pub version: String,
    pub component_type: String,
}

/// A dependency edge from the CycloneDX `dependencies` array.
///
/// Encodes "from_ref depends on each of to_refs." This is NOT derived
/// from iterating the flat `components` list — that list is an inventory.
/// This is the actual tree structure the SBOM generator recorded.
/// If the generator omits this key, the graph degrades gracefully to
/// a flat node list with no edges, which is still useful for inventory queries.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct DependencyEdge {
    pub from_ref: String,
    pub to_refs: Vec<String>,
}

/// The full graph fragment extracted from a CycloneDX SBOM document.
///
/// Carries everything the build processor needs to write `Package` nodes
/// and `DEPENDS_ON` edges in one pass, without re-fetching or re-parsing
/// the SBOM file. The `artifact_content_hash` is the join key to the
/// `ArtifactProduced` event that announced the SBOM file itself.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct SbomGraphFragment {
    /// sha256 content hash of the SBOM file.
    /// Join key to `ArtifactProduced` — not the package identity.
    pub artifact_content_hash: String,
    /// Filename as observed on disk. For debugging, not identity.
    pub filename: String,
    /// The root package this SBOM describes.
    /// None when the SBOM is missing `metadata.component` — spec-noncompliant
    /// but it happens. The processor degrades gracefully.
    pub root: Option<PackageRef>,
    /// Flat inventory of all dependency nodes.
    pub components: Vec<PackageRef>,
    /// Dependency tree edges from the CycloneDX `dependencies` array.
    pub edges: Vec<DependencyEdge>,
}

/// Payload for a raw pipeline artifact that was produced during a build.
///
/// Covers SBOMs, ELF binaries, test reports, scan results, OCI manifest
/// bundles, and anything else that is content-addressed by file hash.
/// OCI container images use [`ProvenanceEvent::ContainerImageCreated`] instead,
/// because they carry richer metadata (layers, config digest, OS/arch) and
/// have different downstream graph relationships.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct ArtifactProducedPayload {
    /// sha256:<hex> computed by `content-hash-file` in the pipeline.
    /// Primary key for `PipelineArtifact` nodes in the graph.
    pub artifact_content_hash: String,
    /// Free string describing the artifact kind:
    /// "sbom", "elf-binary", "test-report", "scan-report", "oci-manifest-bundle", etc.
    pub artifact_type: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub content_type: String,
    #[serde(default)]
    pub build_id: Option<String>,
}

/// Payload for a compiled binary that has been linked to its source package and SBOM.
///
/// The `binding_digest` is a cryptographic attestation:
/// `sha256(binary_hash:cargo_toml_hash:cargo_lock_hash:source_tree_hash)`
/// Recorded for audit — not used for graph structure.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct BinaryLinkedPayload {
    /// Content hash of the compiled binary.
    pub binary_content_hash: String,
    /// Filename of the binary (e.g. "polar-build-processor").
    pub binary_name: String,
    /// Purl of the package this binary was compiled from.
    /// Join key to the `Package` node created by `SbomAnalyzed`.
    pub root_purl: String,
    /// Content hash of the SBOM that describes this package's deps.
    /// Join key to the `Sbom` node created by `SbomAnalyzed`.
    pub sbom_content_hash: String,
    /// Attestation binding digest, if computed.
    #[serde(default)]
    pub binding_digest: String,
}

/// Payload for an OCI container image that was built locally and is available as a tarball.
///
/// Emitted immediately after `nix build` completes — before any registry push.
/// The `config_digest` is the stable content identity across registries.
/// The manifest digest (assigned by the registry after push) arrives later
/// via `ArtifactProduced` with `artifact_type: "oci-image"`.
///
/// Non-image OCI artifacts (manifest bundles, Helm charts, Flux packages)
/// use `ArtifactProduced`, not this type.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct ContainerImageCreatedPayload {
    pub image_name: String,
    pub tarball_hash: String,
    pub config_digest: String,
    pub layers: Vec<OciLayerEntry>,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub entrypoint: String,
    #[serde(default)]
    pub cmd: String,
    #[serde(default)]
    pub repo_tags: Vec<String>,
    /// Post-push registry manifest digest. Absent on the pre-push emission.
    #[serde(default)]
    pub digest: Option<String>,
    /// Post-push remote ref. Absent on the pre-push emission.
    #[serde(default)]
    pub uri: Option<String>,
}

// ── ProvenanceEvent ────────────────────────────────────────────────────────────

/// Canonical provenance event enum for the Polar system.
///
/// Represents all observable facts that agents record about the software
/// supply chain — CI pipeline executions, artifact production, SBOM analysis,
/// OCI artifact discovery and resolution, Kubernetes workload observations, etc.
///
/// ## Wire formats
///
/// - Rust agents serialize this directly via rkyv and publish to the broker.
///   Zero-copy deserialization on the consumer side.
/// - Nushell pipeline stages emit JSON matching variant shapes. The supervisor
///   deserializes with `serde_json::from_slice::<ProvenanceEvent>`.
///
/// ## Routing
///
/// All events flow through `polar.provenance.events`. Each agent subscribes
/// and handles the variants it owns. Unrecognized variants hit the `Ignored`
/// arm rather than a wildcard — the compiler will force a decision when new
/// variants are added.
///
/// ## Identity
///
/// `build_id` lives only on variants that genuinely have one — CI pipeline
/// events. k8s and registry observations have no build context and must not
/// fabricate one.
#[derive(Debug, Archive, RkyvSerialize, RkyvDeserialize, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProvenanceEvent {
    // ── Sentinel ───────────────────────────────────────────────────────────────
    /// Explicit no-op. Supervisors use this to discard variants they don't
    /// handle without a wildcard arm — forces a compiler decision when new
    /// variants are added.
    Ignored,

    // ── CI pipeline lifecycle ──────────────────────────────────────────────────
    // Emitted by instrumented CI pipeline stages (nushell or Rust).
    // Consumed by the build processor to project BuildJob and BuildStage nodes.
    // build_id lives on these variants because only pipeline events have one.
    /// A CI pipeline job has started executing.
    ///
    /// Creates the `BuildJob` anchor node. All subsequent lifecycle events
    /// for this build are correlated via `build_id`. This is the first event
    /// in the build execution lifecycle.
    ExecutionStarted {
        /// Stable correlation ID for this build execution.
        /// Set to `CI_JOB_ID` in GitLab CI; assigned by the normalizer otherwise.
        build_id: String,
        commit_sha: String,
        ref_name: String,
        repo_url: String,
        /// None when not reliably available — honest null beats fabricated identity.
        triggered_by: Option<String>,
        backend: Option<BackendIdentity>,
    },

    /// A named stage within a build execution has started.
    StageStarted {
        build_id: String,
        stage_name: String,
        /// Scoped to the build — (build_id, stage_id) must be unique.
        stage_id: String,
    },

    /// A named stage within a build execution has completed.
    StageCompleted {
        build_id: String,
        stage_name: String,
        stage_id: String,
        duration_secs: u64,
        outcome: StageOutcome,
    },

    /// A build execution completed successfully.
    ExecutionCompleted {
        build_id: String,
        duration_secs: u64,
    },

    /// A build execution failed.
    ///
    /// `stage` is a free string rather than an enum because external CI systems
    /// won't map to internal failure vocabulary.
    ExecutionFailed {
        build_id: String,
        reason: String,
        stage: Option<String>,
    },

    /// A build execution was cancelled.
    ExecutionCancelled {
        build_id: String,
        reason: Option<String>,
    },

    /// A vulnerability was found during a build scan.
    ///
    /// Keyed on `identifier` so the same CVE/GHSA found across multiple builds
    /// converges to one `Vulnerability` node. `FOUND_VULNERABILITY` edges on
    /// `BuildJob` record which builds saw it; `FOUND_IN` links it to the
    /// specific artifact if the scanner attributed it.
    VulnerabilityFound {
        build_id: String,
        severity: String,
        identifier: String,
        /// Content hash of the artifact the vuln was found in, if known at emit time.
        in_artifact: Option<String>,
    },

    // ── Artifact domain ────────────────────────────────────────────────────────
    // Emitted by instrumented pipeline stages or Rust agents.
    // Consumed by the build processor to project artifact nodes and edges.
    // build_id is carried on the payload structs so the processor can write
    // PRODUCED edges without losing execution context.
    /// A raw pipeline artifact was produced.
    /// See [`ArtifactProducedPayload`] for field semantics.
    ArtifactProduced(ArtifactProducedPayload),

    /// An SBOM was analyzed and its dependency graph extracted.
    /// See [`SbomGraphFragment`] for field semantics.
    SbomAnalyzed(SbomGraphFragment),

    /// A compiled binary was linked to its source package and SBOM.
    /// See [`BinaryLinkedPayload`] for field semantics.
    BinaryLinked(BinaryLinkedPayload),

    /// An OCI container image was built and is available as a local tarball.
    /// See [`ContainerImageCreatedPayload`] for field semantics.
    ContainerImageCreated(ContainerImageCreatedPayload),

    // ── Discovery events ───────────────────────────────────────────────────────
    // Emitted by observer agents (GitLab, k8s, registry scanners).
    // Consumed by the resolver, which verifies claims and emits resolution events.
    /// A generic unclassified artifact was discovered.
    ///
    /// Currently near no-op — GitLab package registries don't expose enough
    /// metadata to be actionable. Retained for future use when package registry
    /// introspection improves.
    ArtifactDiscovered { name: String, url: String },

    /// An OCI artifact was discovered in a registry or pipeline.
    ///
    /// Triggers the resolver to fetch the manifest and emit
    /// [`ProvenanceEvent::OCIArtifactResolved`].
    OCIArtifactDiscovered { uri: String },

    /// A container image reference was discovered — in a GitLab pipeline,
    /// a Kubernetes pod spec, or a registry listing.
    ///
    /// Triggers the resolver to dereference the URI and emit
    /// [`ProvenanceEvent::ImageRefResolved`].
    ImageRefDiscovered { uri: String },

    /// A new OCI registry was discovered.
    OCIRegistryDiscovered { hostname: String },

    /// An OCI artifact was created by a build and pushed to a registry.
    ///
    /// The bridge event between build execution and registry artifact. Only the
    /// component that performed the push can emit this honestly — it is the only
    /// point in the system that simultaneously knows the commit SHA, pipeline
    /// context, and the digest of what was pushed.
    OCIArtifactCreated {
        /// Content-addressed digest — the primary join key.
        digest: String,
        /// Full registry URI including digest.
        uri: String,
        /// The build that produced this artifact, if known.
        build_id: Option<String>,
        /// Source commit this artifact was built from.
        commit_sha: Option<String>,
        /// Repository the source came from.
        repo_url: Option<String>,
        /// Media type from the OCI manifest.
        media_type: Option<String>,
        observed_at: i64,
    },

    // ── Resolution events ──────────────────────────────────────────────────────
    // Emitted by the resolver after verifying claims from discovery events.
    // Consumed by the build processor to create verified artifact nodes.
    /// The resolver successfully fetched and verified an OCI artifact manifest.
    ///
    /// `manifest_data` is the raw manifest bytes — the build processor
    /// deserializes these to extract layer descriptors and media type.
    OCIArtifactResolved {
        uri: String,
        digest: String,
        manifest_data: Vec<u8>,
        registry: String,
    },

    /// A previously discovered image reference was resolved to a content-addressed digest.
    ImageRefResolved {
        uri: String,
        digest: String,
        media_type: String,
    },

    // ── Kubernetes observations ────────────────────────────────────────────────
    // Emitted by the k8s observer agent.
    // Consumed by the build processor to link workloads to artifacts.
    /// A pod container was observed using a specific image reference.
    PodContainerUsesImage {
        pod_uid: String,
        container_name: String,
        image_ref: String,
    },

    // ── Build discovery ────────────────────────────────────────────────────────
    /// A build pipeline or job run was discovered by an external observer.
    ///
    /// Distinct from [`ProvenanceEvent::ExecutionStarted`] — this comes from
    /// an observer (e.g. GitLab API polling) rather than from within the
    /// execution boundary. Less authoritative but useful for systems that are
    /// not instrumented with the pipeline library.
    BuildDiscovered {
        pipeline_id: String,
        artifact_ids: Vec<String>,
    },
}
