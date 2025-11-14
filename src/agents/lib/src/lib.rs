use reqwest::{Certificate, Client, ClientBuilder};
use serde::{Deserialize, Serialize};
use std::io::Read;
use tracing::{debug, info};

/// name of the agent responsible receiving provenance events related to discovery of certain artifacts.
/// Including container images and software bill of materials.
pub const PROVENANCE_DISCOVERY_TOPIC: &str = "polar.provenance.discovery";
pub const PROVENANCE_LINKER_TOPIC: &str = "polar.provenance.linker";

pub const DISPATCH_ACTOR: &str = "DISPATCH";
pub const TRANSACTION_FAILED_ERROR: &str = "Expected to start a transaction with the graph";
pub const QUERY_COMMIT_FAILED: &str = "Error committing transaction to graph";
pub const QUERY_RUN_FAILED: &str = "Error running query on the graph.";
pub const UNEXPECTED_MESSAGE_STR: &str = "Received unexpected message!";

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
    /// `id`: Deterministic identifier for the image reference.
    ///        Typically a UUIDv5 derived from the normalized image URI.
    /// `uri`: The canonical image reference string (e.g. `"ghcr.io/org/app:1.2.3"`).
    ImageRefDiscovered { id: String, uri: String },

    /// Emitted when a previously discovered image reference has been
    /// resolved (validated via Skopeo or registry metadata) and now includes
    /// content-addressable digest information.
    ///
    /// `id`: Same deterministic identifier used in `ImageRefDiscovered`.
    /// `digest`: Verified content digest (e.g. `"sha256:abc123..."`).
    /// `media_type`: MIME type of the image manifest (e.g. `"application/vnd.oci.image.manifest.v1+json"`).
    ImageRefResolved {
        id: String,
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
    pub fn image_ref(uri: &str, id: Option<String>) -> Self {
        let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        Self::ImageRefDiscovered {
            id,
            uri: uri.to_string(),
        }
    }

    /// constructor to generate a new image reference event, granting a UUID unless one is provided.
    pub fn sbom_ref(uri: &str, id: Option<String>) -> Self {
        let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        Self::SbomDiscovered {
            artifact_id: id,
            uri: uri.to_string(),
        }
    }
}

#[derive(Debug)]
pub enum DispatcherMessage {
    Dispatch { message: Vec<u8>, topic: String }, // Serialize()
}

pub fn init_logging() {
    let dir = tracing_subscriber::filter::Directive::from(tracing::Level::DEBUG);

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

    let filter = vec![dir]
        .into_iter()
        .fold(EnvFilter::from_default_env(), |filter, directive| {
            filter.add_directive(directive)
        });

    let subscriber = Registry::default().with(filter).with(fmt);
    tracing::subscriber::set_global_default(subscriber).expect("to set global subscriber");

    // TODO: connect to jaeger?
}
/// Helper function to get a web client with optional proxy CA certificate
/// Attempts to find a path to the proxy CA certificate provided by the environment variable PROXY_CA_CERT
pub fn get_web_client() -> Client {
    info!("Attempting to find PROXY_CA_CERT");
    match std::env::var("PROXY_CA_CERT") {
        Ok(path) => {
            let cert_data = get_file_as_byte_vec(&path)
                .expect("Expected to find a proxy CA certificate at {path}");
            let root_cert =
                Certificate::from_pem(&cert_data).expect("Expected {path} to be in PEM format.");

            info!("Found PROXY_CA_CERT at: {path}, Configuring web client...");

            ClientBuilder::new()
                .add_root_certificate(root_cert)
                .use_rustls_tls()
                .build()
                .expect("Expected to build web client with proxy CA certificate")
        }
        Err(e) => {
            debug!("Failed to find PROXY_CA_CERT. {e} Configuring web client without proxy CA certificate...");
            ClientBuilder::new()
                .build()
                .expect("Expected to build web client.")
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
pub fn get_neo_config() -> neo4rs::Config {
    let database_name = std::env::var("GRAPH_DB")
        .expect("Expected to get a neo4j database. GRAPH_DB variable not set.");
    let neo_user = std::env::var("GRAPH_USER").expect("No GRAPH_USER value set for Neo4J.");
    let neo_password =
        std::env::var("GRAPH_PASSWORD").expect("No GRAPH_PASSWORD provided for Neo4J.");
    let neo4j_endpoint = std::env::var("GRAPH_ENDPOINT").expect("No GRAPH_ENDPOINT provided.");
    tracing::info!("Using Neo4j database at {neo4j_endpoint}");

    let config = match std::env::var("GRAPH_CA_CERT") {
        Ok(client_certificate) => {
            tracing::info!(
                "Found GRAPH_CA_CERT at {client_certificate}. Configuring graph client."
            );
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

    config
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
