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
use jira_common::types::JiraGroup;
use polar::graph::controller::{
    GraphController, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property,
};
use ractor::ActorProcessingErr;
use tracing::debug;

/// Builds the single UpsertNodes op for a batch of groups, as a pure
/// function of already-deserialized data -- extracted specifically so
/// this can be unit-tested directly, matching the same pattern used for
/// issues.rs's build_ops_for_issue.
fn build_ops_for_groups(groups: &[JiraGroup]) -> GraphOp {
    let items = groups
        .iter()
        .map(|group| {
            let key = JiraNodeKey::Group {
                name: group.name.to_string(),
            }
            .into_key();
            let props = vec![
                Property(
                    "html".into(),
                    GraphValue::String(group.html.clone().unwrap_or_default()),
                ),
                // Fixed during migration: the original hand-rolled
                // Cypher wrote this under the map key "labels" (plural)
                // but the SET clause read "label" (singular) -- a
                // mismatch that meant group.label was silently always
                // null. Expressing this as a named Property forced an
                // explicit choice rather than letting the mismatch
                // continue silently.
                Property(
                    "label".into(),
                    GraphValue::String(group.label.clone().unwrap_or_default()),
                ),
            ];
            (key, props)
        })
        .collect::<Vec<_>>();

    GraphOp::UpsertNodes { items }
}

/// Writes one decoded batch of groups to the graph.
///
/// Fire-and-forget: an error here means the graph controller actor itself
/// is gone, not that this specific write failed. Real write failures
/// surface asynchronously via GraphSignal::OpFailed -> the supervisor's
/// healthcheck path, not synchronously at this call site.
pub fn handle_groups(
    groups: &[JiraGroup],
    graph_controller: &GraphController,
) -> Result<(), ActorProcessingErr> {
    let op = build_ops_for_groups(groups);
    graph_controller
        .cast(GraphControllerMsg::Op(op))
        .map_err(|e| ActorProcessingErr::from(format!("graph controller unavailable: {e}")))?;
    debug!("Dispatched UpsertNodes op for {} groups.", groups.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar::graph::controller::GraphNodeKey;

    fn test_group(name: &str, html: Option<&str>, label: Option<&str>) -> JiraGroup {
        JiraGroup {
            name: name.to_string(),
            html: html.map(|s| s.to_string()),
            label: label.map(|s| s.to_string()),
        }
    }

    fn key_pattern(key: &dyn GraphNodeKey) -> (String, Vec<(String, neo4rs::BoltType)>) {
        key.cypher_match("n")
    }

    #[test]
    fn builds_one_upsert_nodes_op_with_correct_property_names() {
        // The real regression guard: "label" (singular), not "labels"
        // (plural) -- the exact mismatch that was silently dropping
        // every group's label before this migration.
        let groups = vec![test_group("engineering", Some("<b>eng</b>"), Some("core"))];
        let op = build_ops_for_groups(&groups);

        let GraphOp::UpsertNodes { items } = op else {
            panic!("expected UpsertNodes");
        };
        assert_eq!(items.len(), 1);

        let (key, props) = &items[0];
        let (pattern, params) = key_pattern(key.as_ref());
        assert!(pattern.contains("JiraGroup"));
        assert!(
            params
                .iter()
                .any(|(_, v)| matches!(v, neo4rs::BoltType::String(s) if s.value == "engineering"))
        );

        assert!(props.contains(&Property(
            "html".to_string(),
            GraphValue::String("<b>eng</b>".to_string())
        )));
        assert!(props.contains(&Property(
            "label".to_string(),
            GraphValue::String("core".to_string())
        )));
        assert!(
            !props.iter().any(|p| p.0 == "labels"),
            "must never write the old, buggy plural key"
        );
    }

    #[test]
    fn missing_optional_fields_default_to_empty_string_not_panicking() {
        let groups = vec![test_group("no-metadata", None, None)];
        let op = build_ops_for_groups(&groups);

        let GraphOp::UpsertNodes { items } = op else {
            panic!("expected UpsertNodes");
        };
        let (_, props) = &items[0];
        assert!(props.contains(&Property(
            "html".to_string(),
            GraphValue::String(String::new())
        )));
        assert!(props.contains(&Property(
            "label".to_string(),
            GraphValue::String(String::new())
        )));
    }

    #[test]
    fn multiple_groups_produce_one_item_each_with_distinct_identities() {
        let groups = vec![
            test_group("group-a", None, None),
            test_group("group-b", None, None),
        ];
        let op = build_ops_for_groups(&groups);

        let GraphOp::UpsertNodes { items } = op else {
            panic!("expected UpsertNodes");
        };
        assert_eq!(items.len(), 2);

        let names: Vec<String> = items
            .iter()
            .flat_map(|(key, _)| key_pattern(key.as_ref()).1)
            .filter_map(|(_, v)| match v {
                neo4rs::BoltType::String(s) => Some(s.value),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"group-a".to_string()));
        assert!(names.contains(&"group-b".to_string()));
    }
}
