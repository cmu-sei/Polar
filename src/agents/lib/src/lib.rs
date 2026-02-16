use cassini_client::{TCPClientConfig, TcpClient, TcpClientActor, TcpClientArgs};
use cassini_types::ClientEvent;
use ractor::OutputPort;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use reqwest::{Certificate, Client, ClientBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::Arc;
use tracing::{debug, info};
pub mod graph;
/// wrapper definition for a ractor outputport where a raw message payload and a topic can be piped to a necessary dispatcher
pub type QueueOutput = Arc<OutputPort<(Vec<u8>, String)>>;

/// Canonical serialization error type for the system.
/// Change once, globally.
type RkyvError = rkyv::rancor::Error;

/// name of the agent responsible receiving provenance events related to discovery of certain artifacts.
/// Including container images and software bill of materials.
pub const PROVENANCE_DISCOVERY_TOPIC: &str = "polar.provenance.resolver";
pub const PROVENANCE_LINKER_TOPIC: &str = "polar.provenance.linker";
pub const TRANSACTION_FAILED_ERROR: &str = "Expected to start a transaction with the graph";
pub const QUERY_COMMIT_FAILED: &str = "Error committing transaction to graph";
pub const QUERY_RUN_FAILED: &str = "Error running query on the graph.";
pub const UNEXPECTED_MESSAGE_STR: &str = "Received unexpected message!";
pub const GIT_REPO_DISCOGERY_TOPIC: &str = "polar.git.repositories";
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
            events_output,
        },
        supervisor.into(),
    )
    .await?;

    Ok(tcp_client)
}
/// Helper function to parse a file at a given path and return the raw bytes as a vector
pub fn get_file_as_byte_vec(filename: &String) -> Result<Vec<u8>, std::io::Error> {
    let mut f = std::fs::File::open(&filename)?;

    let metadata = std::fs::metadata(&filename)?;

    let mut buffer = vec![0; metadata.len() as usize];
    f.read(&mut buffer).expect("buffer overflow");

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
    /// Emitted when more generic unclassified artifacts.
    ArtifactDiscovered { name: String, url: String },
    SBOMResolved {
        uid: String,
        name: String,
        sbom: NormalizedSbom,
    },
    /// Emitted when a new OCI registry is discovered
    /// (e.g., observed in a Gitlab, Gitea, Artifactory or registry listing).
    ///
    /// `hostname`: The hostname of the registry (e.g. `"ghcr.io"`).
    OCIRegistryDiscovered { hostname: String },
    /// Emitted when a new OCI artifact is discovered.
    /// observed likely by an agent closest to a registry itself. Either through REST or OCI compatible clients.
    ///
    /// `uri`: The canonical reference string (e.g. `"ghcr.io/org/app:1.2.3"`).
    ///
    OCIArtifactDiscovered { uri: String },
    /// Emitted when a new OCI artifact is resolved.
    ///
    ///
    /// `uri`: The canonical reference string (e.g. `"ghcr.io/org/app:1.2.3"`).
    ///
    OCIArtifactResolved {
        uri: String,
        digest: String,
        media_type: String,
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
    ImageRefDiscovered { uri: String },

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

#[derive(Debug)]
pub enum DispatcherMessage {
    Dispatch { message: Vec<u8>, topic: String }, // Serialize()
}

pub fn init_logging(service_name: String) {
    use opentelemetry::{global, KeyValue};
    use opentelemetry_otlp::Protocol;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::Resource;
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let enable_jaeger = std::env::var("ENABLE_JAEGER_TRACING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if enable_jaeger {
        let endpoint = std::env::var("JAEGER_OTLP_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:4318/v1/traces".to_string());

        if !endpoint.is_empty() {
            if let Ok(exporter) = opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_protocol(Protocol::HttpBinary)
                .with_endpoint(endpoint)
                .build()
            {
                let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                    .with_batch_exporter(exporter)
                    .with_resource(
                        Resource::builder_empty()
                            .with_attributes([KeyValue::new("service.name", service_name.clone())])
                            .build(),
                    )
                    .build();

                global::set_tracer_provider(tracer_provider);
                let tracer = global::tracer(format!("{service_name}.tracing"));

                if tracing_subscriber::registry()
                    .with(filter)
                    .with(tracing_subscriber::fmt::layer())
                    .with(tracing_opentelemetry::layer().with_tracer(tracer))
                    .try_init()
                    .is_err()
                {
                    eprintln!("Logging registry already initialized");
                }
            }
        }
    } else {
        if tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .try_init()
            .is_err()
        {
            eprintln!("Logging registry already initialized");
        }
    }
}

/// Helper function to get a web client with optional proxy CA certificate
/// Attempts to find a path to the proxy CA certificate provided by the environment variable PROXY_CA_CERT
pub fn get_web_client() -> Result<Client, ActorProcessingErr> {
    debug!("Attempting to find PROXY_CA_CERT");
    Ok(match std::env::var("PROXY_CA_CERT") {
        Ok(path) => {
            let cert_data = get_file_as_byte_vec(&path)
                .expect("Expected to find a proxy CA certificate at {path}");
            let root_cert =
                Certificate::from_pem(&cert_data).expect("Expected {path} to be in PEM format.");

            debug!("Found PROXY_CA_CERT at: {path}, Configuring web client...");

            ClientBuilder::new()
                .add_root_certificate(root_cert)
                .use_rustls_tls()
                .build()?
        }
        Err(e) => {
            debug!("Failed to find PROXY_CA_CERT. {e} Configuring web client without proxy CA certificate...");
            ClientBuilder::new().build()?
        }
    })
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
                .build()
                .expect("Expected to build neo4rs configuration")
        }
        Err(_) => neo4rs::ConfigBuilder::default()
            .uri(neo4j_endpoint)
            .user(neo_user)
            .password(neo_password)
            .db(database_name)
            .fetch_size(500)
            .max_connections(10)
            .build()
            .expect("Expected to build neo4rs configuration"),
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

    tracing::trace!("Emitting event {ev:?}");
    client.cast(cassini_client::TcpClientMessage::Publish {
        topic: PROVENANCE_DISCOVERY_TOPIC.to_string(),
        payload,
    })?;

    Ok(())
}
