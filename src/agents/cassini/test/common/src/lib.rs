use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialze, Serialize as SerdeSerialize};
use sha2::{Digest, Sha256};
pub mod client;

/// ---------------------------
/// Environment
/// ---------------------------
#[derive(Debug, Clone, Serialize, Deserialize, Archive, SerdeSerialize, SerdeDeserialze)]
pub struct Environment {
    pub cassini: CassiniConfig,
    pub neo4j: Neo4jConfig,
    pub registry: RegistryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Archive, SerdeSerialize, SerdeDeserialze)]
pub struct CassiniConfig {
    pub tag: String,
    pub ca_cert_path: String,
    pub server_cert_path: String,
    pub server_key_path: String,
    pub jager_host: Option<String>,
    pub log_level: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Archive, SerdeSerialize, SerdeDeserialze)]
pub struct Neo4jConfig {
    pub enable: bool,
    pub version: String,
    pub config: Option<String>, // Optional Text
}

#[derive(Debug, Clone, Serialize, Deserialize, Archive, SerdeSerialize, SerdeDeserialze)]
pub struct RegistryConfig {
    pub enable: bool,
    pub version: String,
    pub port: u64,
}

/// ---------------------------
/// Agent (union)
/// ---------------------------
/// Dhall union = Rust enum with tagged representation.
#[derive(Debug, Clone, Serialize, Deserialize, Archive, SerdeSerialize, SerdeDeserialze)]
pub enum Agent {
    /// Producer definition, contains a path to a .dhall config.
    ProducerAgent,
    SinkAgent,
    ResolverAgent,
    GitlabConsumer,
    KubeConsumer,
    LinkerAgent,
    Observer,
}

/// ---------------------------
/// Action (union with records)
/// ---------------------------
/// Using `serde(tag="type", content="data")` cleanly separates variants.
#[derive(Debug, Clone, Serialize, Deserialize, Archive, SerdeSerialize, SerdeDeserialze)]
pub enum Action {
    /// StartActor
    StartAgent {
        agent: Agent,
        config: Option<String>,
    },

    /// Inject fake GitLab artifact events
    EmitGitlabEvent { json: String },

    /// Inject fake kube API state
    EmitKubeEvent { json: String },

    /// Sleep N seconds
    Sleep(u64),

    /// Assert Cypher query result count
    #[allow(non_snake_case)]
    AssertGraph { cypher: String, expectRows: u64 },

    /// Assert that sink received N events on topic
    AssertEvent { topic: String, count: u64 },
}

/// ---------------------------
/// Phase
/// ---------------------------
#[derive(Debug, Clone, Serialize, Deserialize, Archive, SerdeSerialize, SerdeDeserialze)]
pub struct Phase {
    pub name: String,
    pub actions: Vec<Action>,
}

/// ---------------------------
/// TestPlan
/// ---------------------------
#[derive(Debug, Clone, Serialize, Deserialize, Archive, SerdeSerialize, SerdeDeserialze)]
pub struct TestPlan {
    pub environment: Environment,
    pub phases: Vec<Phase>,
}

pub enum ConnectionState {
    NotContacted,
    Registered { client_id: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct ProducerConfig {
    pub producers: Vec<Producer>,
    // pub payload: serde_json::Value,
}
/// Messaging pattern that mimics user behavior over the network.
#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub enum MessagePattern {
    Burst { idle_time: usize, burst_size: u32 },
    Drip { idle_time: usize },
}
// Config for the supervisor
#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct Producer {
    pub topic: String,
    #[serde(alias = "msgSize")]
    pub msg_size: usize,
    pub duration: u64,
    pub pattern: MessagePattern,
}

#[derive(Debug, Clone, Serialize, Deserialize, Archive)]
pub enum HarnessControllerMessage {
    /// Message sent to clients to give them a token to id themselves by
    ClientRegistered {
        client_id: String,
    },
    ProducerReady {
        client_id: String,
    },
    SinkReady {
        client_id: String,
    },
    StartProducers {
        producers: Vec<Producer>,
    },
    StartSinks {
        topics: Vec<String>,
    },
    Error {
        reason: String,
    },
    /// sent when a serivce receives a shutdown, allowing the harness to finish and do cleanup tasks
    ShutdownAck,
    Shutdown, // stop a service
}

///A general message envelope that gets exchanged between the sink and producer.
/// Dhall already has strong typing, so testers can describe structured values with guaranteed shape.
/// We can import their Dhall config in Rust and convert it to JSON.
/// That JSON can then be wrapped in this envelope (metadata: topic, seqno, checksum if needed, etc).
#[derive(Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct Envelope {
    pub seqno: u64,
    pub data: String,
    pub checksum: String,
}

pub fn compute_checksum(payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    let result = hasher.finalize();
    hex::encode(result) // hex string, e.g. "9c56cc51..."
}

pub fn validate_checksum(payload: &[u8], expected: &str) -> bool {
    let actual = compute_checksum(payload);
    actual == expected
}

pub fn get_instenace_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
