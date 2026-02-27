use rkyv::{Archive, Deserialize, Serialize};

// Messages from Git Watcher (scheduler.in)
#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub enum GitScheduleChange {
    Create { path: String, content: Vec<u8> },   // Dhall as bytes
    Update { path: String, content: Vec<u8> },
    Delete { path: String },
}

// Registration from ad‑hoc agents (scheduler.adhoc)
#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub struct AdhocAgentAnnouncement {
    pub agent_type: String,
    pub version: u64,
    pub default_schedule_json: String, // full schedule as JSON
}

// Notification sent to agents – now includes full schedule JSON
#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub enum ScheduleNotification {
    PermanentUpdate { agent_id: String, schedule_json: String },
    AdhocUpdate { agent_type: String, schedule_json: String },
    EphemeralUpdate { agent_type: String, schedule_json: String },
}
