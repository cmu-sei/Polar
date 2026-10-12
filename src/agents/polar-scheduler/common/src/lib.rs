use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// Where the observer publishes each schedule change; the processor subscribes.
pub const SCHEDULER_IN_TOPIC: &str = "scheduler.in";
/// Where an ad-hoc agent announces itself with a default schedule; the processor subscribes.
pub const SCHEDULER_ADHOC_TOPIC: &str = "scheduler.adhoc";

/// The topic a signed configuration document for `agent` is delivered on. Every builder agent subscribes
/// to its own; the scheduler processor publishes to it.
pub fn configuration_topic(agent: &str) -> String {
    format!("agent.{agent}.configuration")
}

/// A signed configuration document on its way to the agent it is for. The document is carried as it was
/// read from the repository (`{ "payload": .., "signature": .. }`), untouched: the receiver verifies it for
/// itself, and nothing in between is trusted.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub struct ConfigurationNotification {
    pub agent: String,
    pub document_json: String,
}

// Time specification
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub enum TimeSpec {
    Periodic {
        interval: u64,
        unit: TimeUnit,
    },
    Exact {
        timestamp: String,
    },
    Daily {
        hour: u8,
        minute: u8,
        timezone: Option<String>,
    },
    Weekly {
        day: Weekday,
        hour: u8,
        minute: u8,
        timezone: Option<String>,
    },
    Monthly {
        day: u8,
        hour: u8,
        minute: u8,
        timezone: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub enum TimeUnit {
    Minutes,
    Hours,
    Days,
}

#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub enum ScheduleKind {
    Permanent,
    Adhoc,
    Ephemeral,
}

#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub struct ScheduleMetadata {
    pub description: Option<String>,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub enum GitScheduleChange {
    Create { path: String, json: String },
    Update { path: String, json: String },
    Delete { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub struct AdhocAgentAnnouncement {
    pub agent_type: String,
    pub version: u64,
    pub default_schedule_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub enum ScheduleNotification {
    PermanentUpdate {
        agent_id: String,
        schedule_json: String,
    },
    AdhocUpdate {
        agent_type: String,
        schedule_json: String,
    },
    EphemeralUpdate {
        agent_type: String,
        schedule_json: String,
    },
}
