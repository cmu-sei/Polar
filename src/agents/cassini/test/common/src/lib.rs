use rkyv::{Archive, Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct TestPlan {
    pub producers: Vec<ProducerConfig>,
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
pub enum SinkCommand {
    HealthCheck,
    TestPlan(TestPlan), // whole plan or subset
    Stop,               // tell sink to stop
}

#[derive(Debug, Clone, Serialize, Deserialize, Archive)]
pub enum ProducerMessage {
    Ack,
    HealthOk,
    HealthErr { reason: String },
    Ready, // sink has spawned subscribers
    Error { reason: String },
}
