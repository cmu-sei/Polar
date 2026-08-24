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

    /// The issue itself. Referenced as an edge target by nearly every
    /// other variant here (assignee/reporter/creator, type, project,
    /// parent, changelog entries, and every dynamic custom-field node).
    Issue { key: String },

    /// An issue type (e.g. "Bug", "Story"). Keyed on name -- Jira's
    /// issue type payloads don't reliably include a stable id the way
    /// project references do.
    IssueType { name: String },

    /// A single changelog entry. Composite-keyed on (base_id, index)
    /// matching the pre-migration code's own identity exactly.
    ///
    /// `index` is a per-message loop counter, not a stable identifier
    /// from Jira itself -- reasonably idempotent for a straightforward
    /// re-run of the same message (the same changelog structure
    /// produces the same counter sequence), but would produce
    /// duplicate/misaligned nodes if changelog entries were ever
    /// retroactively reordered between two processings of "the same"
    /// history. Preserved as-is rather than redesigned; a genuinely
    /// stable identity here is a separate, bigger question.
    ChangeLog { base_id: String, index: u32 },
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
            JiraNodeKey::Issue { key } => {
                let k = format!("{prefix}_key");
                (
                    format!("({prefix}:JiraIssue {{ key: ${k} }})"),
                    vec![(k, BoltType::String(key.clone().into()))],
                )
            }
            JiraNodeKey::IssueType { name } => {
                let k = format!("{prefix}_name");
                (
                    format!("({prefix}:JiraIssueType {{ name: ${k} }})"),
                    vec![(k, BoltType::String(name.clone().into()))],
                )
            }
            JiraNodeKey::ChangeLog { base_id, index } => {
                let bk = format!("{prefix}_base_id");
                let ik = format!("{prefix}_index");
                (
                    format!(
                        "({prefix}:JiraIssueChangeLog {{ baseId: ${bk}, id: ${ik} }})"
                    ),
                    vec![
                        (bk, BoltType::String(base_id.clone().into())),
                        (ik, BoltType::Integer((*index as i64).into())),
                    ],
                )
            }
        }
    }
}
