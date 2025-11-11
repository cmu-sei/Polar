use serde::{Deserialize, Serialize};

pub const DISPATCH_ACTOR: &str = "DISPATCH";
pub const TRANSACTION_FAILED_ERROR: &str = "Expected to start a transaction with the graph";
pub const QUERY_COMMIT_FAILED: &str = "Error committing transaction to graph";
pub const QUERY_RUN_FAILED: &str = "Error running query on the graph.";
pub const UNEXPECTED_MESSAGE_STR: &str = "Received unexpected message!";
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
