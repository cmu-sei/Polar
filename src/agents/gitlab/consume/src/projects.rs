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

use crate::{subscribe_to_topic, GitlabConsumerArgs, GitlabConsumerState, TRANSACTION_FAILED_ERROR};
use common::types::GitlabData;
use common::PROJECTS_CONSUMER_TOPIC;
use neo4rs::Query;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, info};

pub struct GitlabProjectConsumer;

#[async_trait]
impl Actor for GitlabProjectConsumer {
    type Msg = GitlabData;
    type State = GitlabConsumerState;
    type Arguments = GitlabConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabConsumerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");
        match subscribe_to_topic(args.registration_id, PROJECTS_CONSUMER_TOPIC.to_string()).await {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg =
                    format!("Error subscribing to topic \"{PROJECTS_CONSUMER_TOPIC}\" {e}");
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
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            GitlabData::Projects(projects) => {
                let mut transaction = state
                    .graph
                    .start_txn()
                    .await
                    .expect(TRANSACTION_FAILED_ERROR);
                // Create list of projects
                let project_array = projects.iter()
                    .map(|project| {
                        format!(
                            r#"{{ project_id: "{project_id}", name: "{name}", full_path: "{full_path}", created_at: "{created_at}", last_activity_at: "{last_activity_at}" }}"#,
                            project_id = project.id,
                            name = project.name,
                            full_path = project.full_path,
                            created_at = project.created_at.clone().unwrap_or_default(),
                            last_activity_at = project.last_activity_at.clone().unwrap_or_default(),
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",\n");

                // Here, we write a query that creates additional nodes for the group and namespace of the project
                let cypher_query = format!(
                    "
                    UNWIND [{project_array}] AS project_data
                    MERGE (project:GitlabProject {{ project_id: project_data.project_id }})
                    SET project.name = project_data.name,
                        project.full_path = project_data.full_path,
                        project.created_at = project_data.created_at,
                        project.last_activity_at = project_data.last_activity_at
                    "
                );

                debug!(cypher_query);
                transaction
                    .run(Query::new(cypher_query))
                    .await
                    .expect("Expected to run query.");

                transaction
                    .commit()
                    .await
                    .expect("Expected to commit transaction");
                info!("Transaction committed.");
            }
            _ => (),
        }
        Ok(())
    }
}
