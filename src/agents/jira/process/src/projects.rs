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
use jira_common::types::JiraData;
use polar::graph::controller::{GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use tracing::{debug, error, info};

pub struct JiraProjectConsumer;

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

            let project_count = items.len();

            // Fire-and-forget: a failure here means the graph controller
            // actor itself is gone, not that this specific write failed.
            // Real write failures surface asynchronously via
            // GraphSignal::OpFailed -> the supervisor's healthcheck path,
            // not synchronously at this call site.
            if let Err(e) = state
                .graph_controller
                .cast(GraphControllerMsg::Op(GraphOp::UpsertNodes { items }))
            {
                error!("Failed to send graph op for projects: {e}");
                myself.stop(Some(format!("graph controller unavailable: {e}")));
            } else {
                debug!("Dispatched UpsertNodes op for {project_count} projects.");
            }
        }
        Ok(())
    }
}
