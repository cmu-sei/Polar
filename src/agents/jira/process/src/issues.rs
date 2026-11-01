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

use jira_common::graph::JiraNodeKey;
use jira_common::types::{FieldValue, FirstTierField, JiraIssue, NestedListTypes};
use polar::graph::controller::{
    DynamicNodeKey, GraphController, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey,
    Property,
};
use ractor::ActorProcessingErr;
use tracing::{debug, warn};

/// Sanitizes a raw Jira field name into something safe to embed as a
/// dynamic node label / relationship-type fragment. Best-effort cleanup
/// preserving the pre-migration code's exact label shapes, for
/// continuity with existing graph data. This is NOT itself the safety
/// boundary -- DynamicNodeKey::new validates every label this produces
/// before it can reach a query. A label that still fails validation
/// after this cleanup (a leading digit, a reserved Cypher keyword) is
/// skipped with a warning rather than corrupting a query or crashing
/// the actor -- strictly safer than the original, which had no such
/// protection at all.
fn sanitize_label(raw: &str) -> String {
    raw.replace([' ', '-', '[', ']'], "_")
        .replace(['(', ')', '/', '?'], "")
        .replace('#', "num")
}

/// The property the issue's last update is written to as epoch milliseconds: a number,
/// so times Jira wrote with different UTC offsets compare as instants. The state agent reads its largest value as the
/// observer's cursor (docs/architecture/soft-state.md, "The Jira cursor").
pub const UPDATED_MS: &str = "updated_ms";

/// How Jira writes a time: `2024-01-02T03:04:05.000+0000`.
const JIRA_TIME: &str = "%Y-%m-%dT%H:%M:%S%.f%z";

/// The instant `text` names, in epoch milliseconds, if it is a time as Jira writes one.
pub fn parse_updated_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_str(text.trim(), JIRA_TIME)
        .ok()
        .map(|time| time.timestamp_millis())
}

/// Converts a FirstTierField's scalar variants into a Property with the
/// matching GraphValue type -- preserves real Neo4j integer/boolean
/// types instead of collapsing everything to a string, matching the
/// original code's own (correct) type handling. Returns None for
/// Object/List/Null, matching the original's silent skip of those.
fn first_tier_to_property(key: &str, value: &FirstTierField) -> Option<Property> {
    match value {
        FirstTierField::Option(Some(v)) => {
            Some(Property(key.to_string(), GraphValue::String(v.clone())))
        }
        FirstTierField::Number(v) => Some(Property(
            key.to_string(),
            GraphValue::I64(v.min(i64::MAX as f64) as i64),
        )),
        FirstTierField::Bool(v) => Some(Property(key.to_string(), GraphValue::Bool(*v))),
        _ => None,
    }
}

/// Builds the ops for one JiraUser role relationship (assignee/creator/
/// reporter), pushing them onto `ops`. Matches the original merge_user's
/// call sites and shape exactly, just producing structured GraphOps
/// instead of a hand-built Cypher fragment. The user's own key was
/// previously interpolated directly into Cypher as an unparameterized
/// string literal; it's now a properly parameterized value like every
/// other field in this file.
fn merge_user(ops: &mut Vec<GraphOp>, issue_key: &str, role: &str, field: Option<&FieldValue>) {
    let Some(FieldValue::Object(map)) = field else {
        return;
    };
    let Some(FirstTierField::Option(Some(display))) = map.get("key") else {
        return;
    };

    let user_key = JiraNodeKey::User {
        key: display.clone(),
    };

    let mut props = Vec::new();
    for (key, value) in map {
        if key == "key" {
            continue; // already the node's identity, not a separate property
        }
        if let Some(prop) = first_tier_to_property(key, value) {
            props.push(prop);
        }
    }

    ops.push(GraphOp::UpsertNode {
        key: user_key.clone().into_key(),
        props,
    });
    ops.push(GraphOp::EnsureEdge {
        from: user_key.into_key(),
        to: JiraNodeKey::Issue {
            key: issue_key.to_string(),
        }
        .into_key(),
        rel_type: role.to_string(),
        props: vec![],
    });
}

/// Builds the ops for one dynamic custom-field object (an issue-type,
/// project, parent, or generic custom field whose *label* is derived at
/// runtime from the Jira field name itself). label must already be
/// sanitized; this function validates it via DynamicNodeKey::new and
/// returns None (logging a warning) if it's still unsafe after
/// sanitization, rather than ever letting an invalid label reach a query.
fn dynamic_field_ops(
    ops: &mut Vec<GraphOp>,
    issue_key: &str,
    label: &str,
    key_prop_value: &str,
    props: Vec<Property>,
) {
    let node_label = sanitize_label(&format!("JiraIssue_{label}").to_string());
    let dyn_key = match DynamicNodeKey::new(&node_label, "key", key_prop_value) {
        Ok(k) => k,
        Err(e) => {
            warn!(
                "Skipping dynamic field node -- label '{node_label}' failed \
                 validation even after sanitization: {e}"
            );
            return;
        }
    };
    let rel_type = format!("HAS_{}", label.to_uppercase());
    if let Err(e) = polar::graph::controller::validate_cypher_identifier(&rel_type) {
        warn!("Skipping dynamic field edge -- relationship type '{rel_type}' invalid: {e}");
        return;
    }

    ops.push(GraphOp::UpsertNode {
        key: dyn_key.clone().into_key(),
        props,
    });
    ops.push(GraphOp::EnsureEdge {
        from: JiraNodeKey::Issue {
            key: issue_key.to_string(),
        }
        .into_key(),
        to: dyn_key.into_key(),
        rel_type,
        props: vec![],
    });
}

/// Builds the full set of graph ops for one Jira issue -- every node
/// and edge this file produces, as a pure function of the already-
/// deserialized issue. Extracted from handle() specifically so this
/// can be unit-tested directly against realistic sample data, without
/// needing a running actor or a live Neo4j connection -- there was no
/// existing test coverage for this logic at all before this refactor,
/// despite it being genuinely complex, hand-written, and never
/// verified against real Jira data by anyone still working on this
/// codebase.
fn build_ops_for_issue(issue: &JiraIssue) -> Vec<GraphOp> {
    let mut ops: Vec<GraphOp> = Vec::new();
    let mut issue_props: Vec<Property> = Vec::new();

    for (key, value) in issue.fields.iter() {
        match value {
            FieldValue::OptionValue(Some(v)) => {
                issue_props.push(Property(
                    sanitize_label(&key.clone()),
                    GraphValue::String(v.clone()),
                ));
            }
            FieldValue::Number(v) => {
                issue_props.push(Property(
                    sanitize_label(&key.clone()),
                    GraphValue::I64(v.min(i64::MAX as f64) as i64),
                ));
            }
            FieldValue::Bool(v) => {
                issue_props.push(Property(sanitize_label(&key.clone()), GraphValue::Bool(*v)));
            }
            FieldValue::List(v) if !v.is_empty() => {
                let label = sanitize_label(key);
                for item in v.iter() {
                    match item {
                        NestedListTypes::Option(Some(val)) => {
                            dynamic_field_ops(&mut ops, &issue.key, &label, val, vec![]);
                        }
                        NestedListTypes::Object(the_item) => {
                            let sub_key = if let Some(FirstTierField::Option(Some(name))) =
                                the_item.get("name")
                            {
                                name.clone()
                            } else if let Some(FirstTierField::Option(Some(id))) =
                                the_item.get("id")
                            {
                                id.clone()
                            } else {
                                issue.key.clone()
                            };

                            let mut props = Vec::new();
                            for (attribute, value) in the_item {
                                if let Some(prop) = first_tier_to_property(attribute, value) {
                                    props.push(prop);
                                }
                            }

                            dynamic_field_ops(&mut ops, &issue.key, &label, &sub_key, props);
                        }
                        _ => {}
                    }
                }
            }
            FieldValue::Object(v) => {
                if key == "Assignee" {
                    merge_user(
                        &mut ops,
                        &issue.key,
                        "ASSIGNED",
                        issue.fields.get("Assignee"),
                    );
                } else if key == "Creator" {
                    merge_user(&mut ops, &issue.key, "CREATED", issue.fields.get("Creator"));
                } else if key == "Reporter" {
                    merge_user(
                        &mut ops,
                        &issue.key,
                        "REPORTED",
                        issue.fields.get("Reporter"),
                    );
                } else if key == "Issue Type" {
                    if let Some(FirstTierField::Option(Some(name))) = v.get("name") {
                        let mut props = Vec::new();
                        for (subkey, value) in v {
                            if subkey == "name" {
                                continue;
                            }
                            if let Some(prop) = first_tier_to_property(subkey, value) {
                                props.push(prop);
                            }
                        }
                        let type_key = JiraNodeKey::IssueType { name: name.clone() };
                        ops.push(GraphOp::UpsertNode {
                            key: type_key.clone().into_key(),
                            props,
                        });
                        ops.push(GraphOp::EnsureEdge {
                            from: JiraNodeKey::Issue {
                                key: issue.key.clone(),
                            }
                            .into_key(),
                            to: type_key.into_key(),
                            rel_type: "HAS_TYPE".to_string(),
                            props: vec![],
                        });
                    }
                } else if key == "Project" {
                    if let Some(FirstTierField::Option(Some(project_id))) = v.get("id") {
                        let mut props = Vec::new();
                        for (subkey, value) in v {
                            if subkey == "id" {
                                continue;
                            }
                            if let Some(prop) = first_tier_to_property(subkey, value) {
                                props.push(prop);
                            }
                        }
                        let project_key = JiraNodeKey::Project {
                            project_id: project_id.clone(),
                        };
                        ops.push(GraphOp::UpsertNode {
                            key: project_key.clone().into_key(),
                            props,
                        });
                        ops.push(GraphOp::EnsureEdge {
                            from: JiraNodeKey::Issue {
                                key: issue.key.clone(),
                            }
                            .into_key(),
                            to: project_key.into_key(),
                            rel_type: "PART_OF".to_string(),
                            props: vec![],
                        });
                    } else {
                        warn!(
                            "Issue {}: Project field has no 'id' -- skipping PART_OF edge \
                             rather than create a non-converging, name-keyed duplicate",
                            issue.key
                        );
                    }
                } else if key == "parent" {
                    if let Some(FirstTierField::Option(Some(parent_key))) = v.get("name") {
                        ops.push(GraphOp::EnsureEdge {
                            from: JiraNodeKey::Issue {
                                key: issue.key.clone(),
                            }
                            .into_key(),
                            to: JiraNodeKey::Issue {
                                key: parent_key.clone(),
                            }
                            .into_key(),
                            rel_type: "CHILD_OF".to_string(),
                            props: vec![],
                        });
                    }
                } else {
                    let sub_key = if let Some(FirstTierField::Option(Some(name))) = v.get("name") {
                        name.clone()
                    } else if let Some(FirstTierField::Option(Some(id))) = v.get("id") {
                        id.clone()
                    } else {
                        issue.key.clone()
                    };

                    let label = sanitize_label(key);
                    let mut props = Vec::new();
                    for (attribute, value) in v {
                        if let Some(prop) = first_tier_to_property(attribute, value) {
                            props.push(prop);
                        }
                    }

                    dynamic_field_ops(&mut ops, &issue.key, &label, &sub_key, props);
                }
            }
            _ => {}
        }
    }

    // The system field `updated`, as the observer passed it beside the fields (jira_common::SYSTEM_UPDATED_KEY):
    // never a display name, which is the site's language.
    match &issue.system_updated {
        Some(text) => match parse_updated_ms(text) {
            Some(ms) => issue_props.push(Property(UPDATED_MS.to_string(), GraphValue::I64(ms))),
            None => debug!(
                "Issue {}: its system field `updated` is not a time as Jira writes one, so no {UPDATED_MS} is written",
                issue.key
            ),
        },
        None => debug!(
            "Issue {}: the observer gave no system field `updated`, so no {UPDATED_MS} is written",
            issue.key
        ),
    }

    ops.insert(
        0,
        GraphOp::UpsertNode {
            key: JiraNodeKey::Issue {
                key: issue.key.clone(),
            }
            .into_key(),
            props: issue_props,
        },
    );

    let mut counter: u32 = 0;
    for base_item in &issue.changelog.histories {
        let author = base_item
            .author
            .as_ref()
            .and_then(|a| a.identity())
            .unwrap_or("No one")
            .to_string();

        for item in &base_item.items {
            let mut cl_props = vec![
                Property("author".to_string(), GraphValue::String(author.clone())),
                Property(
                    "created".to_string(),
                    GraphValue::String(base_item.created.clone()),
                ),
                Property("field".to_string(), GraphValue::String(item.field.clone())),
                Property(
                    "fieldtype".to_string(),
                    GraphValue::String(item.fieldtype.clone()),
                ),
            ];
            if let Some(from) = &item.from {
                cl_props.push(Property(
                    "from".to_string(),
                    GraphValue::String(from.clone()),
                ));
            }
            if let Some(from_string) = &item.from_string {
                cl_props.push(Property(
                    "fromString".to_string(),
                    GraphValue::String(from_string.clone()),
                ));
            }
            if let Some(to) = &item.to {
                cl_props.push(Property("to".to_string(), GraphValue::String(to.clone())));
            }
            if let Some(to_string) = &item.to_string {
                cl_props.push(Property(
                    "toString".to_string(),
                    GraphValue::String(to_string.clone()),
                ));
            }

            let cl_key = JiraNodeKey::ChangeLog {
                base_id: base_item.id.clone(),
                index: counter,
            };
            ops.push(GraphOp::UpsertNode {
                key: cl_key.clone().into_key(),
                props: cl_props,
            });
            ops.push(GraphOp::EnsureEdge {
                from: JiraNodeKey::Issue {
                    key: issue.key.clone(),
                }
                .into_key(),
                to: cl_key.into_key(),
                rel_type: "Transitioned".to_string(),
                props: vec![],
            });

            counter += 1;
        }
    }

    ops
}

/// Writes one decoded issue (the raw JSON the observer sent) to the graph
/// as a single transaction. A payload that does not deserialize is logged
/// and skipped, not fatal.
///
/// Fire-and-forget: an error here means the graph controller actor itself
/// is gone, not that this specific write failed. Real write failures
/// surface asynchronously via GraphSignal::OpFailed -> the supervisor's
/// healthcheck path, not synchronously at this call site.
pub fn handle_issue(
    issue_json: &str,
    graph_controller: &GraphController,
) -> Result<(), ActorProcessingErr> {
    let issue: JiraIssue = match serde_json::from_str(issue_json) {
        Ok(i) => i,
        Err(e) => {
            warn!("Failed to deserialize issue: {e}");
            return Ok(());
        }
    };
    debug!("Processing issue: {}", &issue.key);

    let ops = build_ops_for_issue(&issue);
    debug!("Dispatching {} ops for issue {}", ops.len(), issue.key);

    graph_controller
        .cast(GraphControllerMsg::Op(GraphOp::Transaction { ops }))
        .map_err(|e| {
            ActorProcessingErr::from(format!(
                "graph controller unavailable while writing issue {}: {e}",
                issue.key
            ))
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jira_common::types::{JiraAuthor, JiraHistory, JiraIssueChangeLog, JiraItemHistory};
    use polar::graph::controller::GraphNodeKey;
    use std::collections::HashMap;

    fn empty_issue(key: &str) -> JiraIssue {
        JiraIssue {
            expand: String::new(),
            id: "10000".to_string(),
            self_url: String::new(),
            key: key.to_string(),
            fields: HashMap::new(),
            rendered_fields: None,
            system_updated: None,
            changelog: JiraIssueChangeLog {
                start_at: 0.0,
                max_results: 0.0,
                total: 0.0,
                histories: vec![],
            },
        }
    }

    fn test_author(key: &str) -> JiraAuthor {
        JiraAuthor {
            name: Some(key.to_string()),
            key: Some(key.to_string()),
            email_address: Some(format!("{key}@example.com")),
            display_name: Some(key.to_string()),
            active: Some(true),
            time_zone: Some("UTC".to_string()),
            ..JiraAuthor::default()
        }
    }

    /// The authors a changelog entry records: what `ops` writes as each entry's `author`, in order.
    fn changelog_authors(ops: &[GraphOp]) -> Vec<String> {
        ops.iter()
            .filter_map(|op| match op {
                GraphOp::UpsertNode { props, .. } => props.iter().find(|p| p.0 == "author"),
                _ => None,
            })
            .filter_map(|p| match &p.1 {
                GraphValue::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    /// An issue whose history was written by authors the site shows no email for, as Apache's Jira sends them: it
    /// decodes, and each change records who made it.
    #[test]
    fn an_issue_whose_changelog_authors_have_no_email_is_written_with_its_history() {
        let issue: JiraIssue = serde_json::from_value(serde_json::json!({
            "expand": "operations,versionedRepresentations,editmeta,changelog,renderedFields",
            "id": "12442326", "self": "https://issues.apache.org/jira/rest/api/2/issue/12442326", "key": "COMDEV-4",
            "fields": { "Summary": "a question" },
            "changelog": { "startAt": 0, "maxResults": 2, "total": 2, "histories": [
                { "id": "12793179", "created": "2009-12-04T11:38:05.962+0000",
                  "author": { "self": "https://issues.apache.org/jira/rest/api/2/user?username=sri_kumar_4u",
                              "name": "sri_kumar_4u", "key": "sri_kumar_4u", "avatarUrls": {},
                              "displayName": "sri kumar ", "active": false, "timeZone": "Etc/UTC" },
                  "items": [{ "field": "Link", "fieldtype": "jira", "from": null, "fromString": null,
                              "to": "XERCESJ-977", "toString": "This issue relates to XERCESJ-977" }] },
                { "id": "12793186", "created": "2009-12-04T12:22:31.383+0000",
                  "author": { "name": "rgardler" },
                  "items": [{ "field": "status", "fieldtype": "jira", "from": "1", "fromString": "Open",
                              "to": "6", "toString": "Closed" }] }
            ] }
        }))
        .unwrap();
        let ops = build_ops_for_issue(&issue);
        // The issue, and an entry and its edge for each of the two changes.
        assert_eq!(ops.len(), 5);
        assert_eq!(changelog_authors(&ops), ["sri_kumar_4u", "rgardler"]);
    }

    /// Pulls the rendered Cypher pattern + params out of a GraphOp's
    /// key(s), so tests can assert on the real thing GraphNodeKey
    /// produces rather than needing PartialEq on a trait object.
    fn key_pattern(key: &dyn GraphNodeKey) -> (String, Vec<(String, neo4rs::BoltType)>) {
        key.cypher_match("n")
    }

    #[test]
    fn scalar_fields_become_issue_properties() {
        let mut issue = empty_issue("PROJ-1");
        issue.fields.insert(
            "Summary".to_string(),
            FieldValue::OptionValue(Some("Fix the thing".to_string())),
        );
        issue
            .fields
            .insert("Story Points".to_string(), FieldValue::Number(5.0));
        issue
            .fields
            .insert("Flagged".to_string(), FieldValue::Bool(true));

        let ops = build_ops_for_issue(&issue);

        // Exactly one op: the issue node itself. None of these three
        // field types produce related nodes/edges.
        assert_eq!(ops.len(), 1);
        let GraphOp::UpsertNode { key, props } = &ops[0] else {
            panic!("expected UpsertNode");
        };
        let (pattern, _) = key_pattern(key.as_ref());
        assert!(pattern.contains("JiraIssue"));

        assert!(props.contains(&Property(
            "Summary".to_string(),
            GraphValue::String("Fix the thing".to_string())
        )));
        assert!(props.contains(&Property("Story_Points".to_string(), GraphValue::I64(5))));
        assert!(props.contains(&Property("Flagged".to_string(), GraphValue::Bool(true))));
    }

    /// The issue node's properties in `ops`.
    fn issue_props(ops: &[GraphOp]) -> &[Property] {
        let GraphOp::UpsertNode { props, .. } = &ops[0] else {
            panic!("expected the issue's UpsertNode first");
        };
        props
    }

    fn updated(text: &str) -> JiraIssue {
        let mut issue = empty_issue("PROJ-9");
        issue.fields.insert(
            "Updated".to_string(),
            FieldValue::OptionValue(Some(text.to_string())),
        );
        issue.system_updated = Some(text.to_string());
        issue
    }

    #[test]
    fn the_last_update_is_written_as_epoch_milliseconds_beside_its_text() {
        let ops = build_ops_for_issue(&updated("2024-01-02T03:04:05.000+0000"));
        let props = issue_props(&ops);
        assert!(props.contains(&Property(
            "Updated".to_string(),
            GraphValue::String("2024-01-02T03:04:05.000+0000".to_string())
        )));
        assert!(props.contains(&Property(
            UPDATED_MS.to_string(),
            GraphValue::I64(1_704_164_645_000)
        )));
    }

    #[test]
    fn times_written_with_different_offsets_are_compared_as_instants() {
        // The same instant, from a site in UTC and one at UTC-4.
        assert_eq!(
            parse_updated_ms("2024-01-02T03:04:05.000+0000"),
            parse_updated_ms("2024-01-01T23:04:05.000-0400")
        );
        // As text the later one sorts first; as instants it is later.
        let utc = parse_updated_ms("2024-01-02T03:04:05.000+0000").unwrap();
        let later_at_minus_four = parse_updated_ms("2024-01-01T23:30:00.000-0400").unwrap();
        assert!("2024-01-01T23:30:00.000-0400" < "2024-01-02T03:04:05.000+0000");
        assert!(later_at_minus_four > utc);
        assert_eq!(later_at_minus_four - utc, (25 * 60 + 55) * 1000);
        // Milliseconds are kept; seconds without them are read too.
        assert_eq!(
            parse_updated_ms("2024-01-02T03:04:05.123+0000"),
            Some(1_704_164_645_123)
        );
        assert_eq!(
            parse_updated_ms("2024-01-02T03:04:05+0000"),
            Some(1_704_164_645_000)
        );
    }

    #[test]
    fn an_issue_with_no_time_or_one_that_does_not_read_gets_no_milliseconds() {
        for text in [
            "",
            "yesterday",
            "2024-01-02",
            "2024-01-02T03:04:05.000",
            "2024-13-02T03:04:05.000+0000",
        ] {
            assert_eq!(parse_updated_ms(text), None, "{text}");
            let ops = build_ops_for_issue(&updated(text));
            assert!(
                issue_props(&ops).iter().all(|p| p.0 != UPDATED_MS),
                "{text}"
            );
        }
        let ops = build_ops_for_issue(&empty_issue("PROJ-10"));
        assert!(issue_props(&ops).iter().all(|p| p.0 != UPDATED_MS));
    }

    #[test]
    fn the_time_comes_from_the_system_field_the_observer_passed_and_not_from_a_display_name() {
        // A German site: the display name is "Aktualisiert", and a custom field is named "Updated".
        let issue: JiraIssue = serde_json::from_value(serde_json::json!({
            "expand": "", "id": "1", "self": "", "key": "PROJ-11",
            "fields": {
                "Aktualisiert": "2024-01-02T03:04:05.000+0000",
                "Updated": "2030-01-01T00:00:00.000+0000"
            },
            "changelog": { "startAt": 0.0, "maxResults": 0.0, "total": 0.0, "histories": [] },
            "polar_updated": "2024-01-02T03:04:05.000+0000"
        }))
        .unwrap();
        let ops = build_ops_for_issue(&issue);
        assert!(issue_props(&ops).contains(&Property(
            UPDATED_MS.to_string(),
            GraphValue::I64(1_704_164_645_000)
        )));
        // Without it, a field named "Updated" is not read for the cursor.
        let mut without = updated("2024-01-02T03:04:05.000+0000");
        without.system_updated = None;
        let ops = build_ops_for_issue(&without);
        assert!(issue_props(&ops).iter().all(|p| p.0 != UPDATED_MS));
    }

    proptest::proptest! {
        /// Any instant, written as Jira writes it at any whole-minute offset, reads back as that instant; so two
        /// instants keep their order whatever offsets they were written at.
        #[test]
        fn a_time_written_at_any_offset_reads_back_as_its_instant(
            a in 0i64..4_102_444_800_000,
            b in 0i64..4_102_444_800_000,
            offset_a in -840i32..=840,
            offset_b in -840i32..=840,
        ) {
            let write = |ms: i64, minutes: i32| {
                let zone = chrono::FixedOffset::east_opt(minutes * 60).unwrap();
                chrono::DateTime::from_timestamp_millis(ms)
                    .unwrap()
                    .with_timezone(&zone)
                    .format("%Y-%m-%dT%H:%M:%S%.3f%z")
                    .to_string()
            };
            let (text_a, text_b) = (write(a, offset_a), write(b, offset_b));
            proptest::prop_assert_eq!(parse_updated_ms(&text_a), Some(a));
            proptest::prop_assert_eq!(parse_updated_ms(&text_b), Some(b));
            proptest::prop_assert_eq!(
                parse_updated_ms(&text_a).cmp(&parse_updated_ms(&text_b)),
                a.cmp(&b)
            );
        }
    }

    #[test]
    fn assignee_produces_user_node_and_assigned_edge() {
        let mut issue = empty_issue("PROJ-2");
        let mut assignee = HashMap::new();
        assignee.insert(
            "key".to_string(),
            FirstTierField::Option(Some("jdoe".to_string())),
        );
        assignee.insert(
            "displayName".to_string(),
            FirstTierField::Option(Some("Jane Doe".to_string())),
        );
        issue
            .fields
            .insert("Assignee".to_string(), FieldValue::Object(assignee));

        let ops = build_ops_for_issue(&issue);

        // issue node + (user UpsertNode + ASSIGNED EnsureEdge)
        assert_eq!(ops.len(), 3);

        let GraphOp::UpsertNode { key, props } = &ops[1] else {
            panic!("expected UpsertNode for the user");
        };
        let (pattern, params) = key_pattern(key.as_ref());
        assert!(pattern.contains("JiraUser"));
        assert!(
            params
                .iter()
                .any(|(_, v)| matches!(v, neo4rs::BoltType::String(s) if s.value == "jdoe"))
        );
        // "key" itself must not also appear as a property -- it's the
        // node's identity, not a separate attribute.
        assert!(!props.iter().any(|p| p.0 == "key"));
        assert!(props.contains(&Property(
            "displayName".to_string(),
            GraphValue::String("Jane Doe".to_string())
        )));

        let GraphOp::EnsureEdge { rel_type, .. } = &ops[2] else {
            panic!("expected EnsureEdge for the assignment");
        };
        assert_eq!(rel_type, "ASSIGNED");
    }

    #[test]
    fn list_field_with_scalar_items_creates_one_node_per_item() {
        let mut issue = empty_issue("PROJ-3");
        issue.fields.insert(
            "Labels".to_string(),
            FieldValue::List(vec![
                NestedListTypes::Option(Some("backend".to_string())),
                NestedListTypes::Option(Some("urgent".to_string())),
            ]),
        );

        let ops = build_ops_for_issue(&issue);

        // issue node + 2x (UpsertNode + EnsureEdge) = 5
        assert_eq!(ops.len(), 5);

        let mut seen_labels = vec![];
        for op in &ops[1..] {
            if let GraphOp::UpsertNode { key, .. } = op {
                let (pattern, params) = key_pattern(key.as_ref());
                assert!(pattern.contains("JiraIssue_Labels"));
                for (_, v) in params {
                    if let neo4rs::BoltType::String(s) = v {
                        seen_labels.push(s.value.clone());
                    }
                }
            }
        }
        assert!(seen_labels.contains(&"backend".to_string()));
        assert!(seen_labels.contains(&"urgent".to_string()));
    }

    #[test]
    fn list_field_object_item_prefers_name_over_id_over_issue_key() {
        // name present -- should win
        let mut with_name = HashMap::new();
        with_name.insert(
            "name".to_string(),
            FirstTierField::Option(Some("has-name".to_string())),
        );
        with_name.insert(
            "id".to_string(),
            FirstTierField::Option(Some("should-be-ignored".to_string())),
        );

        // only id present
        let mut with_id_only = HashMap::new();
        with_id_only.insert(
            "id".to_string(),
            FirstTierField::Option(Some("has-id".to_string())),
        );

        // neither present -- falls back to issue.key
        let with_neither: HashMap<String, FirstTierField> = HashMap::new();

        let mut issue = empty_issue("PROJ-4");
        issue.fields.insert(
            "Components".to_string(),
            FieldValue::List(vec![
                NestedListTypes::Object(with_name),
                NestedListTypes::Object(with_id_only),
                NestedListTypes::Object(with_neither),
            ]),
        );

        let ops = build_ops_for_issue(&issue);

        let mut seen_keys = vec![];
        for op in &ops {
            if let GraphOp::UpsertNode { key, .. } = op {
                let (pattern, params) = key_pattern(key.as_ref());
                if pattern.contains("JiraIssue_Components") {
                    for (_, v) in params {
                        if let neo4rs::BoltType::String(s) = v {
                            seen_keys.push(s.value.clone());
                        }
                    }
                }
            }
        }
        assert!(seen_keys.contains(&"has-name".to_string()));
        assert!(!seen_keys.contains(&"should-be-ignored".to_string()));
        assert!(seen_keys.contains(&"has-id".to_string()));
        assert!(seen_keys.contains(&"PROJ-4".to_string()));
    }

    #[test]
    fn issue_type_produces_type_node_and_has_type_edge_without_duplicating_name() {
        let mut issue = empty_issue("PROJ-5");
        let mut issue_type = HashMap::new();
        issue_type.insert(
            "name".to_string(),
            FirstTierField::Option(Some("Bug".to_string())),
        );
        issue_type.insert("subtask".to_string(), FirstTierField::Bool(false));
        issue
            .fields
            .insert("Issue Type".to_string(), FieldValue::Object(issue_type));

        let ops = build_ops_for_issue(&issue);
        assert_eq!(ops.len(), 3);

        let GraphOp::UpsertNode { key, props } = &ops[1] else {
            panic!("expected UpsertNode for the issue type");
        };
        let (pattern, _) = key_pattern(key.as_ref());
        assert!(pattern.contains("JiraIssueType"));
        assert!(!props.iter().any(|p| p.0 == "name"));
        assert!(props.contains(&Property("subtask".to_string(), GraphValue::Bool(false))));

        let GraphOp::EnsureEdge { rel_type, .. } = &ops[2] else {
            panic!("expected EnsureEdge");
        };
        assert_eq!(rel_type, "HAS_TYPE");
    }

    #[test]
    fn project_with_id_converges_with_projects_rs_node_shape() {
        let mut issue = empty_issue("PROJ-6");
        let mut project = HashMap::new();
        project.insert(
            "id".to_string(),
            FirstTierField::Option(Some("10042".to_string())),
        );
        project.insert(
            "name".to_string(),
            FirstTierField::Option(Some("Platform".to_string())),
        );
        issue
            .fields
            .insert("Project".to_string(), FieldValue::Object(project));

        let ops = build_ops_for_issue(&issue);
        assert_eq!(ops.len(), 3);

        let GraphOp::UpsertNode { key, .. } = &ops[1] else {
            panic!("expected UpsertNode for the project");
        };
        let (pattern, params) = key_pattern(key.as_ref());
        // Must be project_id-keyed, matching JiraNodeKey::Project exactly
        // (the same identity projects.rs's own consumer already uses) --
        // this is the real regression guard for the two-different-nodes
        // bug fixed in this migration.
        assert!(pattern.contains("JiraProject"));
        assert!(
            params
                .iter()
                .any(|(_, v)| matches!(v, neo4rs::BoltType::String(s) if s.value == "10042"))
        );

        let GraphOp::EnsureEdge { rel_type, .. } = &ops[2] else {
            panic!("expected EnsureEdge");
        };
        assert_eq!(rel_type, "PART_OF");
    }

    #[test]
    fn project_without_id_produces_no_ops_at_all() {
        // The real regression guard: no id means no PART_OF edge and no
        // second, name-keyed, non-converging JiraProject node -- not a
        // silently-created wrong node.
        let mut issue = empty_issue("PROJ-7");
        let mut project = HashMap::new();
        project.insert(
            "name".to_string(),
            FirstTierField::Option(Some("Platform".to_string())),
        );
        issue
            .fields
            .insert("Project".to_string(), FieldValue::Object(project));

        let ops = build_ops_for_issue(&issue);

        // Only the issue node itself -- nothing Project-related at all.
        assert_eq!(ops.len(), 1);
    }

    #[test]
    fn parent_produces_child_of_edge_to_the_named_issue() {
        let mut issue = empty_issue("PROJ-9");
        let mut parent = HashMap::new();
        parent.insert(
            "name".to_string(),
            FirstTierField::Option(Some("PROJ-1".to_string())),
        );
        issue
            .fields
            .insert("parent".to_string(), FieldValue::Object(parent));

        let ops = build_ops_for_issue(&issue);
        // issue node + CHILD_OF edge (no separate node upsert for the
        // parent -- it's referenced by key, matching how every other
        // JiraIssue reference in this file works)
        assert_eq!(ops.len(), 2);

        let GraphOp::EnsureEdge { rel_type, to, .. } = &ops[1] else {
            panic!("expected EnsureEdge");
        };
        assert_eq!(rel_type, "CHILD_OF");
        let (pattern, params) = key_pattern(to.as_ref());
        assert!(pattern.contains("JiraIssue"));
        assert!(
            params
                .iter()
                .any(|(_, v)| matches!(v, neo4rs::BoltType::String(s) if s.value == "PROJ-1"))
        );
    }

    #[test]
    fn generic_custom_object_field_is_treated_as_a_dynamic_node() {
        let mut issue = empty_issue("PROJ-10");
        let mut sprint = HashMap::new();
        sprint.insert(
            "name".to_string(),
            FirstTierField::Option(Some("Sprint 42".to_string())),
        );
        issue
            .fields
            .insert("Sprint Field".to_string(), FieldValue::Object(sprint));

        let ops = build_ops_for_issue(&issue);
        assert_eq!(ops.len(), 3);

        let GraphOp::UpsertNode { key, .. } = &ops[1] else {
            panic!("expected UpsertNode");
        };
        let (pattern, _) = key_pattern(key.as_ref());
        // Spaces sanitized to underscores, matching sanitize_label.
        assert!(pattern.contains("JiraIssue_Sprint_Field"));

        let GraphOp::EnsureEdge { rel_type, .. } = &ops[2] else {
            panic!("expected EnsureEdge");
        };
        assert_eq!(rel_type, "HAS_SPRINT_FIELD");
    }

    #[test]
    fn field_name_invalid_even_after_sanitization_is_skipped_not_panicking() {
        // "." is not in sanitize_label's replacement set, so it survives
        // sanitization unchanged and must be caught by
        // validate_cypher_identifier instead. The real assertion here is
        // that this doesn't panic and produces no ops for this field,
        // rather than ever reaching a query with an unsafe label.
        //
        // (A leading-digit label, tried first, turned out to be a bad
        // test case: dynamic_field_ops always prefixes with "JiraIssue_"
        // before validating, so "2024 Field" becomes "JiraIssue_2024_Field"
        // -- which starts with a letter and passes fine. The prefix
        // itself already protects against that specific failure mode.)
        let mut issue = empty_issue("PROJ-11");
        let mut field = HashMap::new();
        field.insert(
            "name".to_string(),
            FirstTierField::Option(Some("whatever".to_string())),
        );
        issue
            .fields
            .insert("Custom.Field".to_string(), FieldValue::Object(field));

        let ops = build_ops_for_issue(&issue);

        // Only the issue node -- the invalid dynamic field is skipped
        // entirely, not partially applied.
        assert_eq!(ops.len(), 1);
    }

    #[test]
    fn changelog_produces_one_node_and_edge_per_history_item_with_correct_counter() {
        let mut issue = empty_issue("PROJ-12");
        issue.changelog.histories = vec![
            JiraHistory {
                id: "1001".to_string(),
                author: Some(test_author("jdoe")),
                created: "2026-01-01T00:00:00.000Z".to_string(),
                items: vec![
                    JiraItemHistory {
                        field: "status".to_string(),
                        fieldtype: "jira".to_string(),
                        from: Some("1".to_string()),
                        from_string: Some("Open".to_string()),
                        to: Some("3".to_string()),
                        to_string: Some("In Progress".to_string()),
                    },
                    JiraItemHistory {
                        field: "assignee".to_string(),
                        fieldtype: "jira".to_string(),
                        from: None,
                        from_string: None,
                        to: Some("jdoe".to_string()),
                        to_string: Some("Jane Doe".to_string()),
                    },
                ],
            },
            JiraHistory {
                id: "1002".to_string(),
                author: None, // author absent -- must default to "No one"
                created: "2026-01-02T00:00:00.000Z".to_string(),
                items: vec![JiraItemHistory {
                    field: "priority".to_string(),
                    fieldtype: "jira".to_string(),
                    from: Some("Low".to_string()),
                    from_string: None,
                    to: Some("High".to_string()),
                    to_string: None,
                }],
            },
        ];

        let ops = build_ops_for_issue(&issue);

        // issue node + 3 history items x (UpsertNode + EnsureEdge) = 7
        assert_eq!(ops.len(), 7);

        let mut authors_seen = vec![];
        let mut indices_seen = vec![];
        for op in &ops[1..] {
            if let GraphOp::UpsertNode { key, props } = op {
                let (pattern, params) = key_pattern(key.as_ref());
                assert!(pattern.contains("JiraIssueChangeLog"));
                for (name, v) in &params {
                    if name.ends_with("_index")
                        && let neo4rs::BoltType::Integer(i) = v
                    {
                        indices_seen.push(i.value);
                    }
                }
                if let Some(author_prop) = props.iter().find(|p| p.0 == "author")
                    && let GraphValue::String(s) = &author_prop.1
                {
                    authors_seen.push(s.clone());
                }
            }
        }
        assert_eq!(
            indices_seen,
            vec![0, 1, 2],
            "counter must increment across histories, not reset per-history"
        );
        assert!(authors_seen.contains(&"jdoe".to_string()));
        assert!(authors_seen.contains(&"No one".to_string()));
    }

    #[test]
    fn kitchen_sink_issue_produces_the_expected_total_op_count() {
        // A regression tripwire: if this count ever changes
        // unexpectedly, something about how many ops one issue produces
        // has genuinely changed, worth a deliberate look rather than a
        // silent drift.
        let mut issue = empty_issue("PROJ-99");
        issue.fields.insert(
            "Summary".to_string(),
            FieldValue::OptionValue(Some("Kitchen sink".to_string())),
        );
        let mut assignee = HashMap::new();
        assignee.insert(
            "key".to_string(),
            FirstTierField::Option(Some("jdoe".to_string())),
        );
        issue
            .fields
            .insert("Assignee".to_string(), FieldValue::Object(assignee));
        let mut issue_type = HashMap::new();
        issue_type.insert(
            "name".to_string(),
            FirstTierField::Option(Some("Bug".to_string())),
        );
        issue
            .fields
            .insert("Issue Type".to_string(), FieldValue::Object(issue_type));
        let mut project = HashMap::new();
        project.insert(
            "id".to_string(),
            FirstTierField::Option(Some("10042".to_string())),
        );
        issue
            .fields
            .insert("Project".to_string(), FieldValue::Object(project));
        issue.fields.insert(
            "Labels".to_string(),
            FieldValue::List(vec![NestedListTypes::Option(Some("backend".to_string()))]),
        );
        issue.changelog.histories = vec![JiraHistory {
            id: "1001".to_string(),
            author: Some(test_author("jdoe")),
            created: "2026-01-01T00:00:00.000Z".to_string(),
            items: vec![JiraItemHistory {
                field: "status".to_string(),
                fieldtype: "jira".to_string(),
                from: None,
                from_string: None,
                to: None,
                to_string: None,
            }],
        }];

        let ops = build_ops_for_issue(&issue);

        // 1 (issue) + 2 (assignee) + 2 (type) + 2 (project) + 2 (label) + 2 (changelog) = 11
        assert_eq!(ops.len(), 11);
    }
}
