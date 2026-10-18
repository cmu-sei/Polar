//! Graph keys for agents, independent of where they are deployed.
//!
//! `PolarAgent` is the canonical node of an agent: what its code is, named by the name
//! the code declares. It carries nothing about any deployment target. A target's own
//! node for the object that deploys it (for Kubernetes, the kube consumer's
//! `KubernetesPolarAgent`) points at it with `DEPLOYS`, and holds the deployment's facts,
//! its phase history and its instances.
//!
//! ```text
//! (deployment)-[:DEPLOYS]->(:PolarAgent {name})
//! (deployment)-[:INSTANCE_ENDED]->(:AgentInstanceEnding)<-[:ENDED_WITH]-(instance)
//! ```

use crate::graph::controller::GraphNodeKey;
use neo4rs::BoltType;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentNodeKey {
    /// The canonical node of one agent.
    PolarAgent { name: String },
    /// How one instance ended. `instance_id` is the target's identifier for the
    /// instance (a pod's UID), or its name when the identifier is not known.
    InstanceEnding {
        instance_id: String,
        finished_at: String,
    },
}

fn text(name: String, value: &str) -> (String, BoltType) {
    (name, BoltType::String(value.to_string().into()))
}

impl GraphNodeKey for AgentNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        let p = |field: &str| format!("{prefix}_{field}");
        match self {
            AgentNodeKey::PolarAgent { name } => (
                format!("({prefix}:PolarAgent {{ name: ${} }})", p("name")),
                vec![text(p("name"), name)],
            ),
            AgentNodeKey::InstanceEnding {
                instance_id,
                finished_at,
            } => (
                format!(
                    "({prefix}:AgentInstanceEnding {{ instance_id: ${}, finished_at_key: ${} }})",
                    p("instance_id"),
                    p("finished_at")
                ),
                vec![
                    text(p("instance_id"), instance_id),
                    text(p("finished_at"), finished_at),
                ],
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_parameter_carries_the_prefix() {
        let keys = [
            AgentNodeKey::PolarAgent { name: "a".into() },
            AgentNodeKey::InstanceEnding {
                instance_id: "uid".into(),
                finished_at: "1".into(),
            },
        ];
        for key in keys {
            let (pattern, params) = key.cypher_match("m");
            assert!(pattern.starts_with("(m:"), "{pattern}");
            for (name, _) in &params {
                assert!(name.starts_with("m_"), "{name}");
                assert!(
                    pattern.contains(&format!("${name}")),
                    "{pattern} lacks {name}"
                );
            }
        }
    }
}
