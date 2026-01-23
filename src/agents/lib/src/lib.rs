use cassini_types::ClientEvent;
use ractor::{ActorProcessingErr, OutputPort};
use reqwest::{Certificate, Client, ClientBuilder};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::sync::Arc;
use tracing::{debug, info};
/// wrapper definition for a ractor outputport where a raw message payload and a topic can be piped to a necessary dispatcher
pub type QueueOutput = Arc<OutputPort<(Vec<u8>, String)>>;

/// name of the agent responsible receiving provenance events related to discovery of certain artifacts.
/// Including container images and software bill of materials.
pub const PROVENANCE_DISCOVERY_TOPIC: &str = "polar.provenance.resolver";
pub const PROVENANCE_LINKER_TOPIC: &str = "polar.provenance.linker";

pub const DISPATCH_ACTOR: &str = "DISPATCH";
pub const TRANSACTION_FAILED_ERROR: &str = "Expected to start a transaction with the graph";
pub const QUERY_COMMIT_FAILED: &str = "Error committing transaction to graph";
pub const QUERY_RUN_FAILED: &str = "Error running query on the graph.";
pub const UNEXPECTED_MESSAGE_STR: &str = "Received unexpected message!";

pub trait Supervisor {
    /// Helper function to dispatch messages off of message queues to the associated actors within an agent supervision tree.
    /// Payload : a series of raw bytes containing an expected data structure/enum for the agent.
    /// Topci: a string value containing the name of a live actor under the supervisor's supervision tree.
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>);
}

pub enum SupervisorMessage {
    ClientEvent { event: ClientEvent },
}
/// Helper function to parse a file at a given path and return the raw bytes as a vector
pub fn get_file_as_byte_vec(filename: &String) -> Result<Vec<u8>, std::io::Error> {
    let mut f = std::fs::File::open(&filename)?;

    let metadata = std::fs::metadata(&filename)?;

    let mut buffer = vec![0; metadata.len() as usize];
    f.read(&mut buffer).expect("buffer overflow");

    Ok(buffer)
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
    /// Emitted when a new container image reference is discovered
    /// (e.g., observed in a GitLab pipeline, Kubernetes Pod, or registry listing).
    ///
    /// `uri`: The canonical image reference string (e.g. `"ghcr.io/org/app:1.2.3"`).
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

    /// Emitted when an SBOM artifact (CycloneDX, SPDX, etc.) has been
    /// discovered in a build system or artifact store.
    ///
    /// `artifact_id`: Logical identifier of the artifact (e.g. a GitLab artifact ID or UUIDv5 of its path).
    /// `uri`: Resolved download URI or location of the SBOM file.
    SbomDiscovered { artifact_id: String, uri: String },

    /// Emitted when an SBOM has been parsed and its internal dependency graph
    /// extracted, ready to be linked to images or build outputs.
    ///
    /// `component_id`: Logical identifier of the top-level software component (e.g. package name or UUIDv5).
    /// `dependencies`: List of dependency identifiers discovered in the SBOM.
    SbomParsed {
        component_id: String,
        dependencies: Vec<String>,
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

impl ProvenanceEvent {
    /// constructor to generate a new image reference event, granting a UUID unless one is provided.
    pub fn sbom_ref_discovered(uri: &str, id: Option<String>) -> Self {
        let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        Self::SbomDiscovered {
            artifact_id: id,
            uri: uri.to_string(),
        }
    }

    /// constructor to generate a new image resolved event, granting a UUID unless one is provided.
    pub fn image_ref_resolved(
        uri: String,
        id: Option<String>,
        digest: String,
        media_type: String,
    ) -> Self {
        Self::ImageRefResolved {
            uri: uri.to_string(),
            digest,
            media_type,
        }
    }

    //constructor to generate a new image reference event, granting a UUID unless one is provided.
    // pub fn sbom_ref_resolved(uri: &str, id: Option<String>) -> Self {
    //     let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    //     Self::SbomDiscovered {
    //         artifact_id: id,
    //         uri: uri.to_string(),
    //     }
    // }
}

#[derive(Debug)]
pub enum DispatcherMessage {
    Dispatch { message: Vec<u8>, topic: String }, // Serialize()
}

pub fn init_logging() {
    use std::io::stderr;
    use std::io::IsTerminal;
    use tracing_glog::Glog;
    use tracing_glog::GlogFields;
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    let fmt = tracing_subscriber::fmt::Layer::default()
        .with_ansi(stderr().is_terminal())
        .with_writer(std::io::stderr)
        .event_format(Glog::default().with_timer(tracing_glog::LocalTime::default()))
        .fmt_fields(GlogFields::default().compact());

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let subscriber = Registry::default().with(filter).with(fmt);
    tracing::subscriber::set_global_default(subscriber).expect("to set global subscriber");

    // TODO: connect to jaeger?
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
