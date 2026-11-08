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

//! The query the state agent runs for an admitted request, built here and nowhere else: labels and property names
//! cannot be Cypher parameters, so each is checked to be a plain identifier and written backticked, and the asker never
//! sends Cypher (docs/architecture/soft-state.md, "Shape").

use polar::graph::controller::{GraphController, GraphControllerMsg};
use polar_agent_builder::graph_schema::is_identifier;
use soft_state_common::StateReply;
use std::time::Duration;

/// The Cypher for the largest `property` over the nodes labelled `label`, or why the names cannot go into one.
pub fn max_cypher(label: &str, property: &str) -> Result<String, String> {
    if !is_identifier(label) || !is_identifier(property) {
        return Err("a label or property that is not a plain identifier".to_string());
    }
    Ok(format!(
        "MATCH (n:`{label}`) WHERE n.`{property}` IS NOT NULL RETURN max(n.`{property}`) AS value"
    ))
}

/// What the rows of [`max_cypher`]'s query say: the largest value, none when no node has the property, or a refusal
/// when the property is not an integer.
pub fn reply_of(rows: &[neo4rs::Row]) -> StateReply {
    let Some(row) = rows.first() else {
        return StateReply::Value(None);
    };
    match row.get::<Option<i64>>("value") {
        Ok(value) => StateReply::Value(value),
        Err(_) => StateReply::Refused("the property is not an integer".to_string()),
    }
}

/// Reads the largest `property` of the nodes labelled `label` through the graph controller, within `timeout`. A graph
/// that cannot answer is unavailable, and the asker tries again.
pub async fn read_max(
    graph: &GraphController,
    label: &str,
    property: &str,
    timeout: Duration,
) -> StateReply {
    let cypher = match max_cypher(label, property) {
        Ok(cypher) => cypher,
        Err(why) => return StateReply::Refused(why),
    };
    match ractor::rpc::call(
        graph,
        |reply| GraphControllerMsg::Query(neo4rs::Query::new(cypher), reply),
        Some(timeout),
    )
    .await
    {
        Ok(ractor::rpc::CallResult::Success(Ok(rows))) => reply_of(&rows),
        Ok(ractor::rpc::CallResult::Success(Err(e))) => {
            tracing::warn!("the graph could not be read: {e}");
            StateReply::Unavailable("the graph could not be read".to_string())
        }
        Ok(ractor::rpc::CallResult::Timeout) => {
            StateReply::Unavailable("the graph did not answer in time".to_string())
        }
        Ok(ractor::rpc::CallResult::SenderError) | Err(_) => {
            StateReply::Unavailable("the graph controller is not running".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn the_query_names_the_label_and_property_backticked() {
        assert_eq!(
            max_cypher("JiraIssue", "updated_ms").unwrap(),
            "MATCH (n:`JiraIssue`) WHERE n.`updated_ms` IS NOT NULL RETURN max(n.`updated_ms`) AS value"
        );
        for (label, property) in [
            ("Jira`Issue", "x"),
            ("JiraIssue", "x` RETURN 1 //"),
            ("", "x"),
        ] {
            assert!(max_cypher(label, property).is_err(), "{label} {property}");
        }
    }

    proptest! {
        /// Whatever the names, the query is built only from plain identifiers, so nothing the asker sends can close
        /// a backtick, a pattern or a clause.
        #[test]
        fn no_name_that_could_change_the_query_reaches_it(label in "\\PC{0,10}", property in "\\PC{0,10}") {
            match max_cypher(&label, &property) {
                Ok(cypher) => {
                    prop_assert!(is_identifier(&label) && is_identifier(&property));
                    let rest = cypher
                        .replacen(&format!("`{label}`"), "L", 1)
                        .replace(&format!("`{property}`"), "P");
                    prop_assert_eq!(rest, "MATCH (n:L) WHERE n.P IS NOT NULL RETURN max(n.P) AS value");
                }
                Err(_) => prop_assert!(!is_identifier(&label) || !is_identifier(&property)),
            }
        }
    }
}
