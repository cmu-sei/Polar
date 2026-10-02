/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

use jira_common::graph::JiraNodeKey;
use jira_common::types::JiraUser;
use polar::graph::controller::{
    GraphController, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property,
};
use ractor::ActorProcessingErr;
use tracing::debug;

/// Builds the single UpsertNodes op for a batch of users, as a pure
/// function of already-deserialized data -- extracted specifically so
/// this can be unit-tested directly, matching the same pattern used for
/// issues.rs's build_ops_for_issue.
///
/// Keyed on `key` (JiraNodeKey::User), the same identity issues.rs's
/// own merge_user helper uses for Assignee/Creator/Reporter -- both
/// paths converge on the same JiraUser node.
fn build_ops_for_users(users: &[JiraUser]) -> GraphOp {
    let items = users
        .iter()
        .map(|user| {
            let key = JiraNodeKey::User {
                key: user.key.to_string(),
            }
            .into_key();
            let props = vec![
                Property(
                    "html".into(),
                    GraphValue::String(user.html.clone().unwrap_or_default()),
                ),
                Property("name".into(), GraphValue::String(user.name.to_string())),
                Property(
                    "displayName".into(),
                    GraphValue::String(user.display_name.to_string()),
                ),
            ];
            (key, props)
        })
        .collect::<Vec<_>>();

    GraphOp::UpsertNodes { items }
}

/// Writes one decoded batch of users to the graph.
///
/// Fire-and-forget: an error here means the graph controller actor itself
/// is gone, not that this specific write failed. Real write failures
/// surface asynchronously via GraphSignal::OpFailed -> the supervisor's
/// healthcheck path, not synchronously at this call site.
pub fn handle_users(
    users: &[JiraUser],
    graph_controller: &GraphController,
) -> Result<(), ActorProcessingErr> {
    let op = build_ops_for_users(users);
    graph_controller
        .cast(GraphControllerMsg::Op(op))
        .map_err(|e| ActorProcessingErr::from(format!("graph controller unavailable: {e}")))?;
    debug!("Dispatched UpsertNodes op for {} users.", users.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar::graph::controller::GraphNodeKey;

    fn test_user(key: &str, name: &str, html: Option<&str>, display_name: &str) -> JiraUser {
        JiraUser {
            name: name.to_string(),
            key: key.to_string(),
            html: html.map(|s| s.to_string()),
            display_name: display_name.to_string(),
        }
    }

    fn key_pattern(key: &dyn GraphNodeKey) -> (String, Vec<(String, neo4rs::BoltType)>) {
        key.cypher_match("n")
    }

    #[test]
    fn builds_one_upsert_nodes_op_keyed_on_key() {
        let users = vec![test_user("jdoe", "jdoe", Some("<i>bio</i>"), "Jane Doe")];
        let op = build_ops_for_users(&users);

        let GraphOp::UpsertNodes { items } = op else {
            panic!("expected UpsertNodes");
        };
        assert_eq!(items.len(), 1);

        let (key, props) = &items[0];
        let (pattern, params) = key_pattern(key.as_ref());
        // Same identity merge_user (issues.rs) uses for Assignee/
        // Creator/Reporter -- both paths must converge on one node.
        assert!(pattern.contains("JiraUser"));
        assert!(
            params
                .iter()
                .any(|(_, v)| matches!(v, neo4rs::BoltType::String(s) if s.value == "jdoe"))
        );

        assert!(props.contains(&Property(
            "displayName".to_string(),
            GraphValue::String("Jane Doe".to_string())
        )));
        assert!(props.contains(&Property(
            "html".to_string(),
            GraphValue::String("<i>bio</i>".to_string())
        )));
    }

    #[test]
    fn missing_optional_html_defaults_to_empty_string_not_panicking() {
        let users = vec![test_user("nohtml", "nohtml", None, "No HTML")];
        let op = build_ops_for_users(&users);

        let GraphOp::UpsertNodes { items } = op else {
            panic!("expected UpsertNodes");
        };
        let (_, props) = &items[0];
        assert!(props.contains(&Property("html".to_string(), GraphValue::String(String::new()))));
    }

    #[test]
    fn multiple_users_produce_one_item_each_with_distinct_identities() {
        let users = vec![
            test_user("user-a", "user-a", None, "User A"),
            test_user("user-b", "user-b", None, "User B"),
        ];
        let op = build_ops_for_users(&users);

        let GraphOp::UpsertNodes { items } = op else {
            panic!("expected UpsertNodes");
        };
        assert_eq!(items.len(), 2);

        let keys: Vec<String> = items
            .iter()
            .flat_map(|(key, _)| key_pattern(key.as_ref()).1)
            .filter_map(|(_, v)| match v {
                neo4rs::BoltType::String(s) => Some(s.value),
                _ => None,
            })
            .collect();
        assert!(keys.contains(&"user-a".to_string()));
        assert!(keys.contains(&"user-b".to_string()));
    }
}
