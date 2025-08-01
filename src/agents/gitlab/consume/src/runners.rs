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

use crate::{subscribe_to_topic, GitlabConsumerArgs, GitlabConsumerState};
use common::types::{GitlabData, GitlabEnvelope};
use common::RUNNERS_CONSUMER_TOPIC;
use neo4rs::Query;
use polar::{QUERY_COMMIT_FAILED, QUERY_RUN_FAILED, TRANSACTION_FAILED_ERROR};

use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, info};

pub struct GitlabRunnerConsumer;

#[async_trait]
impl Actor for GitlabRunnerConsumer {
    type Msg = GitlabEnvelope;
    type State = GitlabConsumerState;
    type Arguments = GitlabConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabConsumerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");
        //subscribe to topic
        match subscribe_to_topic(
            args.registration_id,
            RUNNERS_CONSUMER_TOPIC.to_string(),
            args.graph_config,
        )
        .await
        {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg = format!("Error starting actor: \"{RUNNERS_CONSUMER_TOPIC}\" {e}");
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
        match state.graph.start_txn().await {
            Ok(mut transaction) => match message.data {
                GitlabData::Runners(runners) => {
                    let runner_data = runners
                        .iter()
                        .map(|runner| {
                            format!(
                                r#"{{
                            runner_id: '{runner_id}',
                            paused: '{paused}',
                            runner_type: '{runner_type:?}',
                            status: '{status:?}',
                            access_level: '{access_level:?}',
                            run_untagged: '{run_untagged}',
                            tag_list: '{tag_list:?}'
                            }}"#,
                                runner_id = runner.id.0,
                                paused = runner.paused,
                                runner_type = runner.runner_type,
                                status = runner.status,
                                access_level = runner.access_level,
                                run_untagged = runner.run_untagged,
                                tag_list = runner.tag_list.clone().unwrap_or_default()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",\n");

                    let cypher_query = format!(
                        r#"
                            UNWIND [{runner_data}] AS runner_data
                            MERGE (runner:GitlabRunner {{ runner_id: runner_data.runner_id }})
                            SET runner.paused = runner_data.paused,
                                runner.runner_type = runner_data.runner_type,
                                runner.status = runner_data.status,
                                runner.access_level = runner_data.access_level,
                                runner.run_untagged = runner_data.run_untagged,
                                runner.tag_list = runner_data.tag_list
                            MERGE (instance: GitlabInstance {{instance_id: "{}" }})
                            WITH instance, runner
                            MERGE (instance)-[:OBSERVED_RUNNER]->(runner)
                        "#,
                        message.instance_id
                    );
                    debug!(cypher_query);
                    if let Err(e) = transaction.run(Query::new(cypher_query)).await {
                        myself.stop(Some(QUERY_RUN_FAILED.to_string()));
                    }

                    if let Err(e) = transaction.commit().await {
                        myself.stop(Some(QUERY_COMMIT_FAILED.to_string()));
                    }
                    info!("Committed transaction to database");
                }
                _ => (),
            },
            Err(e) => myself.stop(Some(format!("{TRANSACTION_FAILED_ERROR}. {e}"))),
        }

        Ok(())
    }
}
