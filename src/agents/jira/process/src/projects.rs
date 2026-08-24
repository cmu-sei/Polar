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

use crate::{JiraConsumerArgs, JiraConsumerState, subscribe_to_topic};

use jira_common::JIRA_PROJECTS_CONSUMER_TOPIC;
use jira_common::graph::JiraNodeKey;
use jira_common::types::{JiraData, JiraProject};
use polar::graph::controller::{GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use tracing::{debug, error, info};

pub struct JiraProjectConsumer;

/// Builds the single UpsertNodes op for a batch of projects, as a pure
/// function of already-deserialized data -- extracted specifically so
/// this can be unit-tested directly, matching the same pattern used for
/// issues.rs's build_ops_for_issue.
///
/// Keyed on project_id (JiraNodeKey::Project), the same identity
/// issues.rs's own Project field handling was fixed to use -- both
/// paths now converge on the same node rather than creating two
/// separate, non-converging JiraProject nodes for the same real
/// project.
fn build_ops_for_projects(projects: &[JiraProject]) -> GraphOp {
    let items = projects
        .iter()
        .map(|project| {
            let key = JiraNodeKey::Project {
                project_id: project.id.to_string(),
            }
            .into_key();
            let props = vec![
                Property(
                    "name".into(),
                    GraphValue::String(project.name.to_string()),
                ),
                Property("key".into(), GraphValue::String(project.key.to_string())),
                Property(
                    "projectTypeKey".into(),
                    GraphValue::String(project.project_type_key.to_string()),
                ),
            ];
            (key, props)
        })
        .collect::<Vec<_>>();

    GraphOp::UpsertNodes { items }
}

#[async_trait]
impl Actor for JiraProjectConsumer {
    type Msg = JiraData;
    type State = JiraConsumerState;
    type Arguments = JiraConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: JiraConsumerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");
        match subscribe_to_topic(
            args.registration_id,
            JIRA_PROJECTS_CONSUMER_TOPIC.to_string(),
            args.graph_controller,
        )
        .await
        {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg =
                    format!("Error starting actor: \"{JIRA_PROJECTS_CONSUMER_TOPIC}\" {e}");
                Err(ActorProcessingErr::from(err_msg))
            }
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("{:?} waiting to consume", myself.get_name());

        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let JiraData::Projects(projects) = message {
            let project_count = projects.len();
            let op = build_ops_for_projects(&projects);

            // Fire-and-forget: a failure here means the graph controller
            // actor itself is gone, not that this specific write failed.
            // Real write failures surface asynchronously via
            // GraphSignal::OpFailed -> the supervisor's healthcheck path,
            // not synchronously at this call site.
            if let Err(e) = state.graph_controller.cast(GraphControllerMsg::Op(op)) {
                error!("Failed to send graph op for projects: {e}");
                myself.stop(Some(format!("graph controller unavailable: {e}")));
            } else {
                debug!("Dispatched UpsertNodes op for {project_count} projects.");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar::graph::controller::GraphNodeKey;

    fn test_project(id: &str, key: &str, name: &str, project_type_key: &str) -> JiraProject {
        JiraProject {
            id: id.to_string(),
            key: key.to_string(),
            name: name.to_string(),
            project_type_key: project_type_key.to_string(),
        }
    }

    fn key_pattern(key: &dyn GraphNodeKey) -> (String, Vec<(String, neo4rs::BoltType)>) {
        key.cypher_match("n")
    }

    #[test]
    fn builds_one_upsert_nodes_op_keyed_on_project_id() {
        let projects = vec![test_project("10042", "PLAT", "Platform", "software")];
        let op = build_ops_for_projects(&projects);

        let GraphOp::UpsertNodes { items } = op else {
            panic!("expected UpsertNodes");
        };
        assert_eq!(items.len(), 1);

        let (key, props) = &items[0];
        let (pattern, params) = key_pattern(key.as_ref());
        // The real regression guard: project_id-keyed, matching exactly
        // what issues.rs's own Project field handling was fixed to use
        // -- both must converge on the same node.
        assert!(pattern.contains("JiraProject"));
        assert!(
            params
                .iter()
                .any(|(_, v)| matches!(v, neo4rs::BoltType::String(s) if s.value == "10042"))
        );

        assert!(props.contains(&Property(
            "name".to_string(),
            GraphValue::String("Platform".to_string())
        )));
        assert!(props.contains(&Property(
            "key".to_string(),
            GraphValue::String("PLAT".to_string())
        )));
        assert!(props.contains(&Property(
            "projectTypeKey".to_string(),
            GraphValue::String("software".to_string())
        )));
    }

    #[test]
    fn multiple_projects_produce_one_item_each_with_distinct_identities() {
        let projects = vec![
            test_project("1", "A", "Project A", "software"),
            test_project("2", "B", "Project B", "business"),
        ];
        let op = build_ops_for_projects(&projects);

        let GraphOp::UpsertNodes { items } = op else {
            panic!("expected UpsertNodes");
        };
        assert_eq!(items.len(), 2);

        let ids: Vec<String> = items
            .iter()
            .flat_map(|(key, _)| key_pattern(key.as_ref()).1)
            .filter_map(|(_, v)| match v {
                neo4rs::BoltType::String(s) => Some(s.value),
                _ => None,
            })
            .collect();
        assert!(ids.contains(&"1".to_string()));
        assert!(ids.contains(&"2".to_string()));
    }
}
