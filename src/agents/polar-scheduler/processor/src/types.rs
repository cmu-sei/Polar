use polar_scheduler_common::{AdhocAgentAnnouncement, GitScheduleChange};

// Internal messages for the processor
#[derive(Debug, Clone)]
pub enum ProcessorMsg {
    GitChange(GitScheduleChange),
    Announcement(AdhocAgentAnnouncement),
    Event { topic: String, payload: Vec<u8> },
}

// Graph node keys (processor‑specific)
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
