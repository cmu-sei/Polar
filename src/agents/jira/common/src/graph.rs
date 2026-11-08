/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

use neo4rs::BoltType;
use polar::graph::controller::{GraphNodeKey, NodeIdentity, NodeKeyVocabulary};

/// Node key vocabulary for Jira-sourced data.
///
/// Merge keys here match exactly what jira/process's four consumers
/// already used as their MERGE targets before this refactor (JiraGroup on
/// name, JiraProject on project_id, JiraUser on key) -- this file doesn't
/// change what identifies a node, only how that identity is expressed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JiraNodeKey {
    Group {
        name: String,
    },
    Project {
        project_id: String,
    },
    User {
        key: String,
    },

    /// The issue itself. Referenced as an edge target by nearly every
    /// other variant here (assignee/reporter/creator, type, project,
    /// parent, changelog entries, and every dynamic custom-field node).
    Issue {
        key: String,
    },

    /// An issue type (e.g. "Bug", "Story"). Keyed on name -- Jira's
    /// issue type payloads don't reliably include a stable id the way
    /// project references do.
    IssueType {
        name: String,
    },

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
    ChangeLog {
        base_id: String,
        index: u32,
    },
}

impl GraphNodeKey for JiraNodeKey {
    fn identity(&self) -> NodeIdentity {
        let s = |v: &String| BoltType::String(v.clone().into());
        match self {
            JiraNodeKey::Group { name } => {
                NodeIdentity::fixed(&["JiraGroup"], vec![("name", s(name))])
            }
            JiraNodeKey::Project { project_id } => {
                NodeIdentity::fixed(&["JiraProject"], vec![("project_id", s(project_id))])
            }
            JiraNodeKey::User { key } => NodeIdentity::fixed(&["JiraUser"], vec![("key", s(key))]),
            JiraNodeKey::Issue { key } => {
                NodeIdentity::fixed(&["JiraIssue"], vec![("key", s(key))])
            }
            JiraNodeKey::IssueType { name } => {
                NodeIdentity::fixed(&["JiraIssueType"], vec![("name", s(name))])
            }
            JiraNodeKey::ChangeLog { base_id, index } => NodeIdentity::fixed(
                &["JiraIssueChangeLog"],
                vec![
                    ("baseId", s(base_id)),
                    ("id", BoltType::Integer((*index as i64).into())),
                ],
            ),
        }
    }
}

impl NodeKeyVocabulary for JiraNodeKey {
    fn every_variant() -> Vec<Self> {
        let s = || "x".to_string();
        let all = vec![
            JiraNodeKey::Group { name: s() },
            JiraNodeKey::Project { project_id: s() },
            JiraNodeKey::User { key: s() },
            JiraNodeKey::Issue { key: s() },
            JiraNodeKey::IssueType { name: s() },
            JiraNodeKey::ChangeLog {
                base_id: s(),
                index: 0,
            },
        ];
        // A variant added to the enum does not compile until it is named here, beside the list it must join.
        for key in &all {
            match key {
                JiraNodeKey::Group { .. }
                | JiraNodeKey::Project { .. }
                | JiraNodeKey::User { .. }
                | JiraNodeKey::Issue { .. }
                | JiraNodeKey::IssueType { .. }
                | JiraNodeKey::ChangeLog { .. } => {}
            }
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar::graph::controller::pattern_problem;

    /// Every Jira key keeps the rules its patterns are composed by, under every variable the graph controller gives
    /// a node (a batch's row, an edge's two ends, a state's three).
    #[test]
    fn every_key_binds_the_variable_it_is_given() {
        for key in JiraNodeKey::every_variant() {
            for prefix in ["n", "m", "a", "b", "res", "stype", "sinst", "from", "to"] {
                assert_eq!(pattern_problem(&key, prefix), None);
            }
        }
    }
}
