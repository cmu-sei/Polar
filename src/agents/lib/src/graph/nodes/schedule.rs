// Graph node keys (processor‑specific)
#[derive(Debug, Clone)]
pub enum ScheduleKey {
    Permanent {
        agent_id: String,
    },
    Adhoc {
        agent_type: String,
    },
    Ephemeral {
        agent_type: String,
    },
    /// A signed configuration item (see docs/architecture/scheduling-kinds.md), keyed by its id.
    Configuration {
        id: String,
    },
    /// What an agent did with a configuration document: one node per outcome, kept as a trail.
    ConfigurationOutcome {
        key: String,
    },
}

impl crate::graph::controller::GraphNodeKey for ScheduleKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, neo4rs::BoltType)>) {
        match self {
            ScheduleKey::Permanent { agent_id } => (
                format!("({prefix}:Schedule:Permanent {{ agent_id: ${prefix}_agent_id }})"),
                vec![(format!("{prefix}_agent_id"), agent_id.clone().into())],
            ),
            ScheduleKey::Adhoc { agent_type } => (
                format!("({prefix}:Schedule:Adhoc {{ agent_type: ${prefix}_agent_type }})"),
                vec![(format!("{prefix}_agent_type"), agent_type.clone().into())],
            ),
            ScheduleKey::Ephemeral { agent_type } => (
                format!("({prefix}:Schedule:Ephemeral {{ agent_type: ${prefix}_agent_type }})"),
                vec![(format!("{prefix}_agent_type"), agent_type.clone().into())],
            ),
            ScheduleKey::Configuration { id } => (
                format!("({prefix}:Schedule:Configuration {{ id: ${prefix}_id }})"),
                vec![(format!("{prefix}_id"), id.clone().into())],
            ),
            ScheduleKey::ConfigurationOutcome { key } => (
                format!("({prefix}:Schedule:ConfigurationOutcome {{ key: ${prefix}_key }})"),
                vec![(format!("{prefix}_key"), key.clone().into())],
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::controller::GraphNodeKey;

    fn keys() -> Vec<ScheduleKey> {
        vec![
            ScheduleKey::Permanent {
                agent_id: "a".into(),
            },
            ScheduleKey::Adhoc {
                agent_type: "t".into(),
            },
            ScheduleKey::Ephemeral {
                agent_type: "t".into(),
            },
            ScheduleKey::Configuration { id: "i".into() },
            ScheduleKey::ConfigurationOutcome { key: "k".into() },
        ]
    }

    /// An edge matches its two ends in one statement, one as `n` and one as `m`; a key that
    /// ignores the variable it is told to use declares `n` twice, and Neo4j refuses the statement.
    #[test]
    fn every_key_names_its_node_by_the_variable_it_is_given() {
        for key in keys() {
            for variable in ["n", "m", "n0", "n1"] {
                let (pattern, params) = key.cypher_match(variable);
                assert!(
                    pattern.starts_with(&format!("({variable}:")),
                    "{key:?} with `{variable}` wrote {pattern}"
                );
                assert!(!params.is_empty());
                // one property map, in Cypher's own braces
                assert_eq!(pattern.matches('{').count(), 1, "{pattern}");
                assert_eq!(pattern.matches('}').count(), 1, "{pattern}");
                for (name, _) in &params {
                    assert!(
                        name.starts_with(&format!("{variable}_")),
                        "{key:?} with `{variable}` has a parameter named {name}, which {pattern} would not use"
                    );
                    assert!(
                        pattern.contains(&format!("${name}")),
                        "{pattern} does not use ${name}"
                    );
                }
            }
        }
    }
}
