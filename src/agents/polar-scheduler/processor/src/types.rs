use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use polar_scheduler_common::{GitScheduleChange, AdhocAgentAnnouncement};

// Time specification (reused from git-agent-common or defined here)
#[derive(Debug, Clone, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub enum TimeSpec {
    Periodic { interval: u64, unit: TimeUnit },
    Exact { timestamp: String }, // ISO 8601
    Daily { hour: u8, minute: u8, timezone: Option<String> },
    Weekly { day: Weekday, hour: u8, minute: u8, timezone: Option<String> },
    Monthly { day: u8, hour: u8, minute: u8, timezone: Option<String> },
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub enum TimeUnit {
    Minutes,
    Hours,
    Days,
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub enum Weekday {
    Mon, Tue, Wed, Thu, Fri, Sat, Sun,
}

// Schedule node as stored in the graph (full config, JSON-friendly)
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct ScheduleNode {
    pub id: String,               // e.g. "permanent/jenkins-observer-1"
    pub kind: ScheduleKind,
    pub agent_id: Option<String>,  // for permanent
    pub agent_type: Option<String>,// for ad-hoc/ephemeral
    pub schedule: TimeSpec,
    pub config: serde_json::Value, // type‑specific config
    pub metadata: ScheduleMetadata,
    pub version: u64,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum ScheduleKind {
    Permanent,
    Adhoc,
    Ephemeral,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct ScheduleMetadata {
    pub description: Option<String>,
    pub owner: Option<String>,
}

// Minimal notification sent to agents
#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub enum ScheduleNotification {
    PermanentUpdate { agent_id: String, schedule_json: String },
    AdhocUpdate { agent_type: String, schedule_json: String },
    EphemeralUpdate { agent_type: String, schedule_json: String },
}

// Internal messages for the processor
#[derive(Debug, Clone)]
pub enum ProcessorMsg {
    GitChange(GitScheduleChange),
    Announcement(AdhocAgentAnnouncement),
    Event { topic: String, payload: Vec<u8> },
}

// Graph node keys
#[derive(Debug, Clone)]
pub enum ScheduleKey {
    Permanent { agent_id: String },
    Adhoc { agent_type: String },
    Ephemeral { agent_type: String },
}

impl polar::graph::GraphNodeKey for ScheduleKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, neo4rs::BoltType)>) {
        match self {
            ScheduleKey::Permanent { agent_id } => (
                format!("(n:Schedule:Permanent {{ agent_id: ${prefix}_agent_id }})"),
                vec![(format!("{prefix}_agent_id"), agent_id.clone().into())],
            ),
            ScheduleKey::Adhoc { agent_type } => (
                format!("(n:Schedule:Adhoc {{ agent_type: ${prefix}_agent_type }})"),
                vec![(format!("{prefix}_agent_type"), agent_type.clone().into())],
            ),
            ScheduleKey::Ephemeral { agent_type } => (
                format!("(n:Schedule:Ephemeral {{ agent_type: ${prefix}_agent_type }})"),
                vec![(format!("{prefix}_agent_type"), agent_type.clone().into())],
            ),
        }
    }
}
