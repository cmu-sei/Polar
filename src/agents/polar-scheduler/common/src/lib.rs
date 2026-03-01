use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

// Time specification
#[derive(Debug, Clone, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub enum TimeSpec {
    Periodic { interval: u64, unit: TimeUnit },
    Exact { timestamp: String },
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

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct ScheduleNode {
    pub id: String,
    pub kind: ScheduleKind,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    pub schedule: TimeSpec,
    pub config: serde_json::Value,
    pub metadata: ScheduleMetadata,
    pub version: u64,
}

// Keep existing messages
#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub enum GitScheduleChange {
    Create { path: String, json: String },
    Update { path: String, json: String },
    Delete { path: String },
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub struct AdhocAgentAnnouncement {
    pub agent_type: String,
    pub version: u64,
    pub default_schedule_json: String,
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub enum ScheduleNotification {
    PermanentUpdate { agent_id: String, schedule_json: String },
    AdhocUpdate { agent_type: String, schedule_json: String },
    EphemeralUpdate { agent_type: String, schedule_json: String },
}
