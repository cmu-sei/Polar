use cassini_client::{TCPClientConfig, TcpClient, TcpClientActor, TcpClientArgs};
use cassini_types::{ClientEvent, WireTraceCtx};
use ractor::OutputPort;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use reqwest::{Certificate, Client, ClientBuilder};
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Debug;
use std::io::Read;
use std::sync::Arc;
use tracing::{debug, info};
pub mod cassini;
pub mod graph;

/// wrapper definition for a ractor outputport where a raw message payload and a topic can be piped to a necessary dispatcher
pub type QueueOutput = Arc<OutputPort<(Vec<u8>, String)>>;

/// Canonical serialization error type for the system.
/// Change once, globally.
pub type RkyvError = rkyv::rancor::Error;

/// name of the agent responsible receiving provenance events related to discovery of certain artifacts.
/// Including container images and software bill of materials.
pub const PROVENANCE_DISCOVERY_TOPIC: &str = "polar.provenance.resolver";
pub const PROVENANCE_LINKER_TOPIC: &str = "polar.provenance.linker";
pub const TRANSACTION_FAILED_ERROR: &str = "Expected to start a transaction with the graph";
pub const QUERY_COMMIT_FAILED: &str = "Error committing transaction to graph";
pub const QUERY_RUN_FAILED: &str = "Error running query on the graph.";
pub const UNEXPECTED_MESSAGE_STR: &str = "Received unexpected message!";
pub const GIT_REPO_DISCOGERY_TOPIC: &str = "polar.git.repositories";
pub const BUILDS_TOPIC_PREFIX: &str = "polar.builds";
pub const ARTIFACT_PRODUCED_SUFFIX: &str = "artifact.produced";
pub const SBOM_RESOLVED_SUFFIX: &str = "sbom.resolved";

pub trait Supervisor {
    /// Helper function to dispatch messages off of message queues to the associated actors within an agent supervision tree.
    /// Payload : a series of raw bytes containing an expected data structure/enum for the agent.
    /// Topci: a string value containing the name of a live actor under the supervisor's supervision tree.
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>);
}

pub enum SupervisorMessage {
    ClientEvent { event: ClientEvent },
}

/// Helper to spawn a TCP client to connect to the message broker, Cassini.
/// CAUTION! There is a known bug in the ractor framework that makes output ports drop messages and freeze under high load.
/// REFERENCE: https://github.com/slawlor/ractor/issues/225
/// Until this is resolved, a workaround is to spawn another actor to serve as a subscriber/forwarder to push messages where they need to go.
/// It's also possible to just have the client attempt to forward any mesages that come from the broker straight to its supervisor, as that's where they
/// always go anyway. For now, we can leave this up to our future selves to decide what's best
#[deprecated = "Functionality moved to TcpClient::spawn()"]
pub async fn spawn_tcp_client<M, F>(
    service_name: &str,
    supervisor: ActorRef<M>,
    map_event: F,
) -> Result<TcpClient, ActorProcessingErr>
where
    M: Send + 'static,
    F: Fn(ClientEvent) -> Option<M> + Send + Sync + 'static,
{
    let events_output = std::sync::Arc::new(OutputPort::default());

    events_output.subscribe(supervisor.clone(), map_event);

    let config = TCPClientConfig::new()?;

    let (tcp_client, _) = Actor::spawn_linked(
        Some(format!("{service_name}.tcp")),
        TcpClientActor,
        TcpClientArgs {
            config,
            registration_id: None,
            events_output: Some(events_output),
            event_handler: None,
        },
        supervisor.into(),
    )
    .await?;

    Ok(tcp_client)
}

/// ---------------------------------------------------------------------------
/// BuildEvent — the wire format for events emitted by nushell pipeline stages.
///
/// This is NOT the same as ProvenanceEvent. ProvenanceEvent is the internal
/// domain enum used by the supervisor and linker. BuildEvent is the wire
/// representation that nushell stages produce as JSON. The distinction
/// matters because:
///
///   1. Rust agents: serialize ProvenanceEvent directly via rkyv.
///      The supervisor deserializes with rkyv::from_bytes and gets
///      the domain enum directly. No BuildEvent intermediary.
///
///   2. Nushell stages: serialize BuildEvent as JSON (because nushell
///      can't produce rkyv archives). The supervisor deserializes with
///      BuildEvent::from_bytes, then calls into_provenance_event() to
///      map into the domain enum.
///
/// Both paths converge at ProvenanceEvent before hitting the linker.
///
/// The payload is a concrete enum with #[serde(tag = "type")], which
/// means serde handles variant dispatch internally. The `type` field
/// that nushell writes (`$payload | merge { type: $subject_suffix }`)
/// maps directly to #[serde(rename = "...")] on each variant. No
/// serde_json::Value intermediary, no manual string matching.
/// ---------------------------------------------------------------------------
/// The wire format produced by nushell pipeline stages.
/// JSON-only — Rust agents bypass this entirely via rkyv.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildEvent {
    pub build_id: String,
    pub stage_exec_id: String,
    pub pipeline_exec_id: String,
    pub observed_at: String,
    pub payload: BuildEventPayload,
}

// ===========================================================================
// Payload struct
//
// Mirrors exactly what the nushell emit produces. The `layers` array
// carries uncompressed diff IDs in stack order — these are the layer
// identities as Nix/Docker sees them, before registry compression.
// ===========================================================================

#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct OciLayerEntry {
    pub order: u32,
    pub diff_id: String,
    #[serde(default)]
    pub tar_path: String,
}

#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct ContainerImageCreatedPayload {
    /// Human-readable name (e.g. "cassini", "kube-observer").
    pub image_name: String,

    /// Content hash of the tarball on disk. This is the identity of
    /// the image as a build artifact before it has a registry digest.
    pub tarball_hash: String,

    /// Digest of the OCI config blob. Stable across re-uploads of
    /// the same image content to different registries.
    pub config_digest: String,

    /// Ordered layer stack with uncompressed diff IDs.
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
}

/// Concrete payload variants that nushell stages can emit.
///
/// Internally tagged on `type` — this matches the nushell emit function's
/// `($payload | merge { type: $subject_suffix })`. Serde reads the `type`
/// field and routes directly to the correct variant. Unknown values produce
/// a deserialization error rather than silently passing through.
///
/// When you add a new `emit-*` function in nushell, add a variant here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BuildEventPayload {
    /// From `emit-sbom-resolved`: the graph projection of a single SBOM.
    #[serde(rename = "sbom.resolved")]
    SbomAnalyzed(SbomGraphFragment),

    /// From `emit-artifact-produced`: provenance record for any build output.
    #[serde(rename = "artifact.produced")]
    ArtifactProduced(ArtifactProducedPayload),

    #[serde(rename = "binary.linked")]
    BinaryLinked(BinaryLinkedPayload),

    #[serde(rename = "container-image.created")]
    ContainerImageCreated(ContainerImageCreatedPayload),
}

#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct BinaryLinkedPayload {
    /// Content hash of the compiled binary.
    pub binary_content_hash: String,

    /// Filename of the binary (e.g. "polar-linker").
    pub binary_name: String,

    /// Purl of the package this binary was built from.
    /// This is the join key to the SBOM's root package node.
    pub root_purl: String,

    /// Content hash of the SBOM that describes this package's deps.
    /// Join key to the Sbom node created by handle_sbom_analyzed.
    pub sbom_content_hash: String,

    /// Attestation binding digest, if computed.
    /// sha256(binary_hash:cargo_toml_hash:cargo_lock_hash:source_tree_hash)
    /// Useful for audit, not used for graph structure.
    #[serde(default)]
    pub binding_digest: String,
}

// ---------------------------------------------------------------------------
// Graph fragment types — the SBOM projection.
//
// These derive both serde (for the nushell JSON path) and rkyv (for when
// a Rust agent constructs and emits an SbomAnalyzed event directly).
// Both serialization paths produce the same logical data.
// ---------------------------------------------------------------------------

#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct PackageRef {
    pub purl: String,
    pub name: String,
    pub version: String,
    pub component_type: String,
}

#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct DependencyEdge {
    pub from_ref: String,
    pub to_refs: Vec<String>,
}

#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct SbomGraphFragment {
    /// Content hash of the SBOM file. Provenance join key:
    /// (Artifact {hash})-[:DESCRIBES]->(Package {purl}).
    /// This is NOT the package identity — that's root.purl.
    pub artifact_content_hash: String,

    /// Filename as observed on disk. For debugging, not identity.
    pub filename: String,

    /// The root package this SBOM describes. None if the SBOM was
    /// missing metadata.component (spec-noncompliant but it happens).
    pub root: Option<PackageRef>,

    /// Flat inventory of all dependency nodes. Each becomes a MERGE
    /// target in Neo4j, keyed on purl.
    pub components: Vec<PackageRef>,

    /// Dependency tree edges from the CycloneDX `dependencies` array.
    /// NOT derived from iterating `components` — that's a flat list
    /// with no parent-child relationships. These are the actual edges.
    pub edges: Vec<DependencyEdge>,
}

#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize,
)]
pub struct ArtifactProducedPayload {
    pub artifact_content_hash: String,
    pub artifact_type: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub content_type: String,
}

// ---------------------------------------------------------------------------
// Deserialization and conversion
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum BuildEventError {
    #[error("json deserialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

impl BuildEvent {
    /// Deserialize from raw JSON bytes.
    ///
    /// Serde's internally-tagged enum handles dispatch on the `type` field
    /// inside `payload`. No manual matching, no Value intermediary.
    pub fn from_bytes(raw: &[u8]) -> Result<Self, BuildEventError> {
        Ok(serde_json::from_slice(raw)?)
    }

    /// Convert this wire-format event into the domain ProvenanceEvent.
    ///
    /// This is the bridge between "what nushell emitted" (BuildEvent)
    /// and "what the supervisor operates on" (ProvenanceEvent). The
    /// mapping is 1:1 — each BuildEventPayload variant corresponds to
    /// exactly one ProvenanceEvent variant that carries the same data.
    pub fn into_provenance_event(self) -> (ProvenanceContext, ProvenanceEvent) {
        let ctx = ProvenanceContext {
            build_id: self.build_id,
            stage_exec_id: self.stage_exec_id,
            pipeline_exec_id: self.pipeline_exec_id,
            observed_at: self.observed_at,
        };

        let event = match self.payload {
            BuildEventPayload::SbomAnalyzed(fragment) => ProvenanceEvent::SbomAnalyzed(fragment),
            BuildEventPayload::ArtifactProduced(payload) => {
                ProvenanceEvent::ArtifactProduced(payload)
            }
            BuildEventPayload::BinaryLinked(payload) => ProvenanceEvent::BinaryLinked(payload),
            BuildEventPayload::ContainerImageCreated(payload) => {
                ProvenanceEvent::ContainerImageCreated(payload)
            }
        };

        (ctx, event)
    }
}

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceContext {
    pub build_id: String,
    pub stage_exec_id: String,
    pub pipeline_exec_id: String,
    pub observed_at: String,
}

/// Helper function to parse a file at a given path and return the raw bytes as a vector
#[deprecated = "Use async_get_file_as_byte_vec instead to avoid blocking."]
pub fn get_file_as_byte_vec(filename: &String) -> Result<Vec<u8>, std::io::Error> {
    let mut f = std::fs::File::open(filename)?;

    let metadata = std::fs::metadata(filename)?;

    let mut buffer = vec![0; metadata.len() as usize];
    f.read_exact(&mut buffer)?;

    Ok(buffer)
}

use tokio::fs;
use tokio::io::AsyncReadExt;

pub async fn async_get_file_as_byte_vec(path: &str) -> Result<Vec<u8>, std::io::Error> {
    let mut file = fs::File::open(path).await?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).await?;
    Ok(buffer)
}

/// Generate a content-addressed UID for an artifact based on its raw bytes.
/// This is the *only* valid way artifacts should acquire identity.
pub fn artifact_uid_from_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();

    // Lowercase hex encoding, stable across languages and systems
    hex::encode(digest)
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    Hash,
    rkyv::Serialize,
    rkyv::Deserialize,
    rkyv::Archive,
)]
pub enum SbomFormat {
    CycloneDx,
    Spdx,
}

impl SbomFormat {
    pub fn as_str(&self) -> &str {
        match self {
            SbomFormat::CycloneDx => "CycloneDx",
            SbomFormat::Spdx => "spdx",
        }
    }
}

#[derive(
    Debug,
    Clone,
    serde::Serialize,
    serde::Deserialize,
    rkyv::Serialize,
    rkyv::Deserialize,
    rkyv::Archive,
)]
pub struct NormalizedComponent {
    pub name: String,
    pub version: String,
    pub purl: String,
    pub component_type: String,
}
// TODO: We probably want a normalized definition of what an SBOM for our purproses here
#[derive(
    Debug,
    Clone,
    serde::Serialize,
    serde::Deserialize,
    rkyv::Serialize,
    rkyv::Deserialize,
    rkyv::Archive,
)]
pub struct NormalizedSbom {
    pub format: SbomFormat,
    pub spec_version: String,
    /// Strongly validated components
    pub components: Vec<NormalizedComponent>,
}

/// A simple event that says a git repo was discovered
///
#[derive(Debug, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub struct GitRepositoryDiscoveredEvent {
    pub http_url: Option<String>,
    pub ssh_url: Option<String>,
}

/// Represents normalized provenance-related events emitted across agents.
/// Each variant communicates new or updated knowledge about entities in the provenance graph.
///
/// IMPORTANT:
/// - `id` fields are **logical identifiers**, not Neo4j node IDs.
/// - These should be **stable** across environments and derivable from upstream data
///   (e.g. image URIs, pipeline IDs, artifact hashes).
/// - Internal Neo4j IDs (`id(node)`) MUST NOT be emitted in events — they are ephemeral.
#[derive(Debug, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub enum ProvenanceEvent {
    ArtifactProduced(ArtifactProducedPayload),
    SbomAnalyzed(SbomGraphFragment),
    BinaryLinked(BinaryLinkedPayload),

    /// Emitted when more generic unclassified artifacts.
    ArtifactDiscovered {
        name: String,
        url: String,
    },
    SBOMResolved {
        uid: String,
        name: String,
        sbom: NormalizedSbom,
    },
    /// The OCIArtifactCreated event is the bridge.
    /// When Cyclops completes a build, it's the only component in the system that simultaneously knows the commit SHA, the project,
    /// the pipeline context from the BuildRequest metadata, and the digest of what was pushed to the registry.
    /// It can emit an event that carries all of that context, and the processor can use it to write edges
    /// that neither the GitLab agent nor the k8s agent could write on their own.
    OCIArtifactCreated {
        /// The content-addressed digest of the artifact.
        /// This is the join key to everything else.
        digest: String,

        /// Full registry URI including digest.
        /// e.g. "registry.internal.example.com/builds/myapp@sha256:abc..."
        uri: String,

        /// The build that produced this artifact, if known.
        /// None if the artifact was pushed outside of Cyclops.
        build_id: Option<String>,

        /// Source commit this artifact was built from.
        commit_sha: Option<String>,

        /// Repository the source came from.
        repo_url: Option<String>,

        /// Media type from the OCI manifest.
        /// Tells consumers what kind of artifact this is.
        media_type: Option<String>,

        observed_at: i64,
    },
    /// Emitted when a new OCI registry is discovered
    /// (e.g., observed in a Gitlab, Gitea, Artifactory or registry listing).
    ///
    /// `hostname`: The hostname of the registry (e.g. `"ghcr.io"`).
    OCIRegistryDiscovered {
        hostname: String,
    },
    /// Emitted when a new OCI artifact is discovered.
    /// observed likely by an agent closest to a registry itself. Either through REST or OCI compatible clients.
    ///
    /// `uri`: The canonical reference string (e.g. `"ghcr.io/org/app:1.2.3"`).
    ///
    OCIArtifactDiscovered {
        uri: String,
    },
    /// Emitted when a new OCI artifact is resolved.
    ///
    ///
    /// `uri`: The canonical reference string (e.g. `"ghcr.io/org/app:1.2.3"`).
    /// `manifest_data`: raw byte payload containing the OCIManifest Structure, to be deserialized and analyzed
    OCIArtifactResolved {
        uri: String,
        digest: String,
        manifest_data: Vec<u8>,
        registry: String,
    },

    /// Emitted when a new container image reference is discovered
    /// (e.g., observed in a GitLab pipeline, Kubernetes Pod, or registry listing).
    ///
    /// `uri`: The canonical image reference string (e.g. `"ghcr.io/org/app:1.2.3"`).

    /// What this means:
    /// “A resolver successfully dereferenced this URI at some point in time and observed an OCI artifact with these properties.”
    /// “The URI is this artifact
    /// “The artifact is an image”
    /// “This is the canonical truth forever”
    /// It means: there exists evidence linking a claim to an artifact. To that end, it does not represent artifact itself in our database
    ImageRefDiscovered {
        uri: String,
    },

    /// Emitted when a previously discovered image reference has been
    /// resolved (validated via Skopeo or registry metadata) and now includes
    /// content-addressable digest information.
    ///
    /// `digest`: Verified content digest (e.g. `"sha256:abc123..."`).
    /// `media_type`: MIME type of the image manifest (e.g. `"application/vnd.oci.image.manifest.v1+json"`).
    ImageRefResolved {
        uri: String,
        digest: String,
        media_type: String,
    },

    ContainerImageCreated(ContainerImageCreatedPayload),

    /// Emitted when a pod container is seen using an image.
    ///
    /// `pod_uid`: Unique identifier of the pod.
    /// `container_name`: Name of the container.
    /// `image_ref`: Reference to the image used by the container.
    PodContainerUsesImage {
        pod_uid: String,
        container_name: String,
        image_ref: String,
    },

    /// Emitted when an SBOM has been parsed and its internal dependency graph
    /// extracted, ready to be linked to images or build outputs.
    ///
    /// `uid`: Logical identifier of the artifact
    /// `sbom`: the normalized sbom instance of the artifact
    /// `name`: filename observed
    SbomResolved {
        uid: String,
        sbom: NormalizedSbom,
        name: String,
    },

    /// Emitted when a new build pipeline or job run has been discovered.
    /// Allows linking pipeline executions, their outputs, and artifacts.
    ///
    /// `pipeline_id`: Stable identifier of the build (e.g. GitLab pipeline ID or UUIDv5 thereof).
    /// `artifact_ids`: Logical IDs of artifacts produced by this build (SBOMs, images, logs, etc.).
    BuildDiscovered {
        pipeline_id: String,
        artifact_ids: Vec<String>,
    },
}

/// Emitted by Polar when it observes a change to a git repository —
/// specifically when a new commit is detected on a tracked branch or ref.
///
/// This is the primary trigger for Cyclops build jobs. The orchestrator
/// consumes this event off the Cassini broker, maps the repository URL
/// to a pipeline image via its repo_mappings config, and submits a
/// build job for the given commit.
///
/// The event is produced by Polar's VCS observation agents and reflects
/// what Polar *observed*, not what was directly reported by a webhook.
/// `observed_at` may therefore lag the actual commit timestamp by the
/// agent's polling interval.
#[derive(Debug, Default, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub struct GitRepositoryUpdatedEvent {
    /// Unique identifier for this event. Used for idempotency — if the
    /// orchestrator receives the same event_id twice (e.g. due to broker
    /// redelivery), it should deduplicate rather than submit two builds.
    pub event_id: String,

    /// HTTPS clone URL of the repository.
    /// e.g. "https://github.com/cmu-sei/Polar.git"
    /// At least one of http_url or ssh_url must be Some.
    pub http_url: Option<String>,

    /// SSH clone URL of the repository.
    /// e.g. "git@github.com:cmu-sei/Polar.git"
    /// Preferred when the identity root agent can issue per-job SSH credentials.
    /// At least one of http_url or ssh_url must be Some.
    pub ssh_url: Option<String>,

    /// Full 40-character SHA of the commit that triggered this event.
    /// Never a branch name or short SHA — those are mutable and ambiguous.
    /// The orchestrator passes this directly to the build job and the
    /// git clone init container.
    pub commit_sha: String,

    /// The ref (branch or tag) on which this commit was observed.
    /// e.g. "refs/heads/main", "refs/tags/v1.2.3"
    /// Informational — the orchestrator builds the commit, not the ref.
    /// Useful for Polar to correlate builds with branch activity in the
    /// knowledge graph.
    pub git_ref: Option<String>,

    /// The repository's default branch name, if known.
    /// e.g. "main", "master"
    /// Allows consumers to distinguish commits on the default branch
    /// from feature branch activity without parsing git_ref.
    pub default_branch: Option<String>,

    /// Display name of the repository, without the host prefix.
    /// e.g. "cmu-sei/Polar"
    /// Used for log messages and as a human-readable identifier in the
    /// knowledge graph. Not used for cloning — use http_url or ssh_url.
    pub repository_name: Option<String>,

    /// Identity of the actor who authored the commit, if available.
    /// Typically the git author email. Polar correlates this with user
    /// identity records in the knowledge graph.
    pub author: Option<String>,

    /// Unix timestamp (seconds since epoch) when Polar's agent observed
    /// this commit. May lag the actual commit time by the agent's polling
    /// interval. Not the git commit timestamp — use commit_sha to retrieve
    /// that from the repository if needed.
    pub observed_at: i64,
}

#[derive(Debug)]
pub enum DispatcherMessage {
    Dispatch { message: Vec<u8>, topic: String }, // Serialize()
}

pub fn init_logging(service_name: String) {
    use opentelemetry::{KeyValue, global};
    use opentelemetry_otlp::Protocol;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::{
        Resource,
        trace::{self, RandomIdGenerator},
    };
    use tracing_subscriber::Layer;
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let enable_jaeger = std::env::var("JAEGER_ENABLE_TRACING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if enable_jaeger {
        let endpoint = std::env::var("JAEGER_OTLP_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:4318/v1/traces".to_string());

        if !endpoint.is_empty()
            && let Ok(exporter) = opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_protocol(Protocol::HttpBinary)
                .with_endpoint(endpoint)
                .build()
        {
            let tracer_provider = trace::SdkTracerProvider::builder()
                .with_batch_exporter(exporter)
                .with_sampler(trace::Sampler::AlwaysOn)
                .with_id_generator(RandomIdGenerator::default())
                .with_resource(
                    Resource::builder()
                        // FIX: Pass service_name directly, not a reference
                        .with_service_name(service_name.clone())
                        .with_attributes([
                            KeyValue::new("service.name", service_name.clone()),
                            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
                        ])
                        .build(),
                )
                .build();

            global::set_tracer_provider(tracer_provider);

            // CRITICAL: Use the SAME tracer name as the broker
            let tracer = global::tracer("cassini-tracing");

            let telemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);

            // Apply the filter separately
            let filtered_telemetry_layer =
                telemetry_layer.with_filter(EnvFilter::from_default_env());

            if tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer())
                .with(filtered_telemetry_layer)
                .try_init()
                .is_err()
            {
                eprintln!("Logging registry already initialized");
            }
        }
    } else {
        let fmt_layer = tracing_subscriber::fmt::layer()
            .pretty() // <-- this is the missing piece
            .with_thread_ids(true)
            .with_target(true)
            .with_file(true)
            .with_line_number(true);

        if tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .try_init()
            .is_err()
        {
            eprintln!("Logging registry already initialized");
        }
    }
}

pub async fn try_get_proxy_ca_cert() -> Option<Vec<u8>> {
    match std::env::var("PROXY_CA_CERT") {
        Ok(path) => {
            info!("Attempting to load a ca certificate at {path}");
            async_get_file_as_byte_vec(&path).await.ok()
        }
        Err(_e) => None,
    }
}

/// Helper function to get a web client with optional proxy CA certificate
/// Attempts to find a path to the proxy CA certificate provided by the environment variable PROXY_CA_CERT
pub fn get_web_client() -> Result<Client, ActorProcessingErr> {
    debug!("Attempting to find PROXY_CA_CERT");
    match std::env::var("PROXY_CA_CERT") {
        Ok(path) => {
            let cert_data = std::fs::read(&path)?;
            let root_cert = Certificate::from_pem(&cert_data)?;

            debug!(
                "Found PROXY_CA_CERT at: {}. Configuring web client...",
                path
            );

            Ok(ClientBuilder::new()
                .add_root_certificate(root_cert)
                .use_rustls_tls()
                .build()?)
        }
        Err(_) => {
            debug!(
                "Failed to find PROXY_CA_CERT. Configuring web client without proxy CA certificate..."
            );
            Ok(ClientBuilder::new().build()?)
        }
    }
}

/// Standard helper fn to get a neo4rs configuration based on environment variables
/// All of the following variables are required fields unless otherwise specified.
/// GRAPH_DB - name of the neo4j database
/// GRAPH_PASSWORD - password credential used to sign in
/// GRAPH_USER - the username to authenticate with the database
/// GRAPH_ENDPOINT - endpoint of the database
/// GRAPH_CA_CERT - an optional proxy CA certificate if needed to connect to the neo4j instance.
pub fn get_neo_config() -> Result<neo4rs::Config, ractor::ActorProcessingErr> {
    let database_name = std::env::var("GRAPH_DB")?;
    let neo_user = std::env::var("GRAPH_USER")?;
    let neo_password = std::env::var("GRAPH_PASSWORD")?;
    let neo4j_endpoint = std::env::var("GRAPH_ENDPOINT")?;
    info!("Using Neo4j database at {neo4j_endpoint}");

    let config = match std::env::var("GRAPH_CA_CERT") {
        Ok(client_certificate) => {
            info!("Found GRAPH_CA_CERT at {client_certificate}. Configuring graph client.");
            neo4rs::ConfigBuilder::default()
                .uri(neo4j_endpoint)
                .user(neo_user)
                .password(neo_password)
                .db(database_name)
                .fetch_size(500)
                .with_client_certificate(client_certificate)
                .max_connections(10)
                .build()?
        }
        Err(_) => neo4rs::ConfigBuilder::default()
            .uri(neo4j_endpoint)
            .user(neo_user)
            .password(neo_password)
            .db(database_name)
            .fetch_size(500)
            .max_connections(10)
            .build()?,
    };

    Ok(config)
}

/// A canonical reference to a container image
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ContainerImageReference {
    pub registry: String,
    pub repository: String,
    pub tag: Option<String>,
    pub digest: Option<String>,
}

impl ContainerImageReference {
    /// Returns a canonical reference to the container image.
    pub fn canonical(&self) -> String {
        match (&self.tag, &self.digest) {
            (Some(tag), _) => format!("{}/{}:{}", self.registry, self.repository, tag),
            (None, Some(digest)) => format!("{}/{}@{}", self.registry, self.repository, digest),
            _ => format!("{}/{}", self.registry, self.repository),
        }
    }
}

pub fn emit_provenance_event(
    ev: ProvenanceEvent,
    client: &TcpClient,
) -> Result<(), ActorProcessingErr> {
    let payload = rkyv::to_bytes::<RkyvError>(&ev)?.to_vec();

    let trace_ctx = WireTraceCtx::from_current_span();
    tracing::trace!("Emitting event {ev:?}");
    client.cast(cassini_client::TcpClientMessage::Publish {
        topic: PROVENANCE_DISCOVERY_TOPIC.to_string(),
        payload,
        trace_ctx,
    })?;

    Ok(())
}
