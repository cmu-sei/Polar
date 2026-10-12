// Graph node keys (processor‑specific)
#[derive(Debug, Clone)]
pub enum ScheduleKey {
    Permanent { agent_id: String },
    Adhoc { agent_type: String },
    Ephemeral { agent_type: String },
    /// A signed configuration item (see docs/architecture/scheduling-kinds.md), keyed by its id.
    Configuration { id: String },
    /// What an agent did with a configuration document: one node per outcome, kept as a trail.
    ConfigurationOutcome { key: String },
}

impl crate::graph::controller::GraphNodeKey for ScheduleKey {
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
            ScheduleKey::Configuration { id } => (
                format!("(n:Schedule:Configuration {{ id: ${prefix}_id }})"),
                vec![(format!("{prefix}_id"), id.clone().into())],
            ),
            ScheduleKey::ConfigurationOutcome { key } => (
                format!("(n:Schedule:ConfigurationOutcome {{ key: ${prefix}_key }})"),
                vec![(format!("{prefix}_key"), key.clone().into())],
            ),
        }
    }
}
