use rkyv::{Archive, Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod client;

pub enum ConnectionState {
    NotContacted,
    Registered { client_id: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct TestPlan {
    pub producers: Vec<ProducerConfig>,
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
pub struct ProducerConfig {
    pub topic: String,
    #[serde(alias = "msgSize")]
    pub msg_size: usize,
    pub duration: u64,
    pub pattern: MessagePattern,
}

#[derive(Debug, Clone, Serialize, Deserialize, Archive)]
pub enum HarnessControllerMessage {
    /// Message sent to clients to give them a token to id themselves by, mostly for convenience
    ClientRegistered(String),
    TestPlanRequest {
        client_id: String,
    },
    TestPlan {
        plan: TestPlan,
    },
    Error {
        reason: String,
    },
    /// sent when a serivce receives a shutdown, allowing the harness to finish and do cleanup tasks
    ShutdownAck,
    Shutdown, // stop a service
}

// #[derive(Debug, Clone, Serialize, Deserialize, Archive)]
// pub enum ProducerMessage {

// }

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
