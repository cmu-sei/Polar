use rkyv::{Archive, Deserialize, Serialize};
use sha2::{Digest, Sha256};

// pub mod client;

/// Messaging pattern that mimics user behavior over the network.
#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub enum MessagePattern {
    Drip {
        idle_time_seconds: u64,
    },
    Burst {
        burst_size: u64,
        idle_time_seconds: u64,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub enum PayloadSpec {
    Fixed(String),
    Random { seed: u64 },
    FromFile { path: String },
    Template { template: String },
}
// Config for the supervisor
#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct ProducerConfig {
    pub topic: String,
    pub msg_size: u64,
    pub message_count: u64,
    pub duration_seconds: u64,
    pub pattern: MessagePattern,
    pub payload: PayloadSpec,
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
