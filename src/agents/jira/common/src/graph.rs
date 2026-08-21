use neo4rs::BoltType;
use polar::graph::controller::GraphNodeKey;

/// Node key vocabulary for Jira-sourced data.
///
/// Merge keys here match exactly what jira/process's four consumers
/// already used as their MERGE targets before this refactor (JiraGroup on
/// name, JiraProject on project_id, JiraUser on key) -- this file doesn't
/// change what identifies a node, only how that identity is expressed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JiraNodeKey {
    Group { name: String },
    Project { project_id: String },
    User { key: String },
}

impl GraphNodeKey for JiraNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            JiraNodeKey::Group { name } => {
                let k = format!("{prefix}_name");
                (
                    format!("({prefix}:JiraGroup {{ name: ${k} }})"),
                    vec![(k, BoltType::String(name.clone().into()))],
                )
            }
            JiraNodeKey::Project { project_id } => {
                let k = format!("{prefix}_project_id");
                (
                    format!("({prefix}:JiraProject {{ project_id: ${k} }})"),
                    vec![(k, BoltType::String(project_id.clone().into()))],
                )
            }
            JiraNodeKey::User { key } => {
                let k = format!("{prefix}_key");
                (
                    format!("({prefix}:JiraUser {{ key: ${k} }})"),
                    vec![(k, BoltType::String(key.clone().into()))],
                )
            }
        }
    }
}
