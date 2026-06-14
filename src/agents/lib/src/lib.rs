pub mod cassini;
pub mod events;
pub mod graph;
pub mod topics;
pub use events::*;

use cassini_client::{
    OfflineBehavior, PublishRequest, TCPClientConfig, TcpClientActor, TcpClientArgs,
    TcpClientMessage,
};
use cassini_types::{ClientEvent, WireTraceCtx};
use ractor::OutputPort;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use reqwest::{Certificate, Client, ClientBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Debug;
use std::io::Read;
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncReadExt;
use tracing::{debug, info};

use crate::cassini::CassiniClient;
use crate::topics::PROVENANCE_DISCOVERY;

// ── Topic constants ────────────────────────────────────────────────────────────

/// Topic the build processor subscribes to for all provenance events.
/// All agents — CI pipeline stages, k8s observer, GitLab observer, resolver —
/// publish canonical `ProvenanceEvent` instances here.
pub const PROVENANCE_LINKER_TOPIC: &str = "polar.provenance.linker";

pub const TRANSACTION_FAILED_ERROR: &str = "Expected to start a transaction with the graph";
pub const QUERY_COMMIT_FAILED: &str = "Error committing transaction to graph";
pub const QUERY_RUN_FAILED: &str = "Error running query on the graph.";
pub const UNEXPECTED_MESSAGE_STR: &str = "Received unexpected message!";
pub const BUILD_PROCESSOR_NAME: &str = "polar.builds.processor";

/// The canonical topic for all provenance events.
/// Replaces the old per-subject topics (polar.builds.artifact.produced, etc.).
pub const BUILD_EVENTS_TOPIC: &str = "polar.provenance.events";

// ── Framework types ────────────────────────────────────────────────────────────

/// Re-export of [`GraphControllerActor::get_neo_config`] as a free function.
///
/// Agents that need a Neo4j connection config can call `polar::get_neo_config()`
/// without importing `GraphControllerActor` directly.
pub fn get_neo_config() -> Result<neo4rs::Config, ractor::ActorProcessingErr> {
    graph::controller::GraphControllerActor::get_neo_config()
}

/// Wrapper for a ractor output port where a raw message payload and topic
/// can be piped to a necessary dispatcher.
pub type QueueOutput = Arc<OutputPort<(Vec<u8>, String)>>;

/// Canonical serialization error type for the system.
pub type RkyvError = rkyv::rancor::Error;

pub trait Supervisor {
    /// Dispatch messages off message queues to associated actors within
    /// an agent supervision tree.
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>);
}

pub enum SupervisorMessage {
    ClientEvent { event: ClientEvent },
}

/// Spawn a TCP client to connect to the Cassini message broker.
///
/// CAUTION: There is a known bug in the ractor framework that makes output
/// ports drop messages and freeze under high load.
/// See: https://github.com/slawlor/ractor/issues/225
#[deprecated = "There is a known bug in the ractor framework that makes output ports drop messages and freeze under high load. Functionality moved to TcpClient::spawn()"]
pub async fn spawn_tcp_client<M, F>(
    service_name: &str,
    supervisor: ActorRef<M>,
    map_event: F,
) -> Result<ActorRef<TcpClientMessage>, ActorProcessingErr>
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

// ── Provenance emission helper ─────────────────────────────────────────────────

/// Serialize and publish a [`ProvenanceEvent`] to the discovery topic via rkyv.
///
/// Used by Rust agents that emit events directly — k8s observer, GitLab observer,
/// resolver. Nushell pipeline stages use `emit-build-event` in `core.nu` instead,
/// which emits JSON to the same topic.
pub fn emit_provenance_event(
    ev: ProvenanceEvent,
    client: &dyn CassiniClient,
) -> Result<(), ActorProcessingErr> {
    let payload = rkyv::to_bytes::<RkyvError>(&ev)?.to_vec();
    let trace_ctx = WireTraceCtx::from_current_span();
    tracing::trace!("Emitting event {ev:?}");

    client.publish(PublishRequest {
        topic: PROVENANCE_DISCOVERY.to_string(),
        payload,
        trace_ctx,
        offline_behavior: OfflineBehavior::Drop,
    })?;

    Ok(())
}

// ── File utilities ─────────────────────────────────────────────────────────────

/// Read a file at a given path and return its raw bytes.
#[deprecated = "Use async_get_file_as_byte_vec instead to avoid blocking."]
pub fn get_file_as_byte_vec(filename: &String) -> Result<Vec<u8>, std::io::Error> {
    let mut f = std::fs::File::open(filename)?;
    let metadata = std::fs::metadata(filename)?;
    let mut buffer = vec![0; metadata.len() as usize];
    f.read_exact(&mut buffer)?;
    Ok(buffer)
}

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
    hex::encode(digest)
}

// ── Web client ─────────────────────────────────────────────────────────────────
// TODO: Generalize to try and read a file from an env var
pub async fn try_get_proxy_ca_cert() -> Option<Vec<u8>> {
    match std::env::var("PROXY_CA_CERT") {
        Ok(path) => {
            info!("Attempting to load a ca certificate at {path}");
            async_get_file_as_byte_vec(&path).await.ok()
        }
        Err(_e) => None,
    }
}

/// Build an HTTP client with optional proxy CA certificate.
///
/// Reads `PROXY_CA_CERT` from the environment. If present, configures
/// the client to trust that certificate. Falls back to default trust roots.
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
                "PROXY_CA_CERT not found. Configuring web client without proxy CA certificate..."
            );
            Ok(ClientBuilder::new().build()?)
        }
    }
}

// ── Domain types ───────────────────────────────────────────────────────────────

/// A canonical reference to a container image.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ContainerImageReference {
    pub registry: String,
    pub repository: String,
    pub tag: Option<String>,
    pub digest: Option<String>,
}

impl ContainerImageReference {
    pub fn canonical(&self) -> String {
        match (&self.tag, &self.digest) {
            (Some(tag), _) => format!("{}/{}:{}", self.registry, self.repository, tag),
            (None, Some(digest)) => format!("{}/{}@{}", self.registry, self.repository, digest),
            _ => format!("{}/{}", self.registry, self.repository),
        }
    }
}

#[derive(Debug)]
pub enum DispatcherMessage {
    Dispatch { message: Vec<u8>, topic: String },
}

// ── SBOM types ─────────────────────────────────────────────────────────────────

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
    pub components: Vec<NormalizedComponent>,
}

// ── Logging ────────────────────────────────────────────────────────────────────

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
                        .with_service_name(service_name.clone())
                        .with_attributes([
                            KeyValue::new("service.name", service_name.clone()),
                            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
                        ])
                        .build(),
                )
                .build();

            global::set_tracer_provider(tracer_provider);

            let tracer = global::tracer("cassini-tracing");
            let telemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);
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
            .pretty()
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
