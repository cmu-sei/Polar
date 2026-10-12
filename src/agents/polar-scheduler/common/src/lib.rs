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

/// Where an agent says what became of each signed configuration document it was given. The scheduler
/// processor subscribes and writes each to the graph.
pub const CONFIGURATION_OUTCOME_TOPIC: &str = "polar-configuration-outcome";

/// The values of [`ConfigurationOutcome::status`].
pub mod outcome {
    pub const APPLIED: &str = "applied";
    /// Verified and valid, but it changed nothing.
    pub const UNCHANGED: &str = "unchanged";
    /// Verified, but its version is not higher than the last applied.
    pub const STALE: &str = "stale";
    /// Verified, but what it asks is not acceptable (a variable that is static, a value that does not
    /// parse, a relation that would break).
    pub const INVALID: &str = "invalid";
    /// Not accepted as a signed document: unsigned, altered, untrusted, not allowed, or for another agent.
    pub const UNVERIFIED: &str = "unverified";
}

/// What an agent did with one signed configuration document. Carries names and reasons, never values.
/// `id` and `version` are as the document claims them, and mean something only when `signer` is set,
/// which it is only for a document that was verified.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub struct ConfigurationOutcome {
    pub agent: String,
    pub id: Option<String>,
    pub version: Option<u64>,
    /// One of the [`outcome`] values.
    pub status: String,
    /// The variables whose value changed, for an applied update.
    pub changed: Vec<String>,
    /// Why not, for anything that was not applied: problems, or the reason it was refused.
    pub detail: Vec<String>,
    /// Who signed it (username or identity, and certificate serial), for a verified document.
    pub signer: Option<String>,
    pub at_unix_secs: u64,
    /// Counts this agent's outcomes since it started, so two in the same second are two events.
    pub sequence: u64,
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
