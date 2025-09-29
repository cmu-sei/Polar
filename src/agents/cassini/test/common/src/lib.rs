use rkyv::{Archive, Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Archive)]
pub enum SinkCommand {
    HealthCheck,
    TestPlan,              // whole plan or subset
    Start(Option<String>), // tell sink to begin consuming, optionally with a registration id to teest session logic.
    Stop,                  // tell sink to stop
}

#[derive(Debug, Clone, Serialize, Deserialize, Archive)]
pub enum ProducerMessage {
    Ack,
    HealthOk,
    HealthErr { reason: String },
    Ready, // sink has spawned subscribers
    Error { reason: String },
}
