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


use crate::{subscribe_to_topic, GitlabConsumerArgs, GitlabConsumerState, QUERY_COMMIT_FAILED, QUERY_RUN_FAILED, TRANSACTION_FAILED_ERROR};
use common::types::GitlabData;
use common::PIPELINE_CONSUMER_TOPIC;
use neo4rs::Query;
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use tokio::net::unix::pipe;
use tracing::{debug, error, field::debug, info, warn};

pub struct GitlabPipelineConsumer;

#[async_trait]
impl Actor for GitlabPipelineConsumer {
    type Msg = GitlabData;
    type State = GitlabConsumerState;
    type Arguments = GitlabConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabConsumerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");
        //subscribe to topic
        match subscribe_to_topic(args.registration_id, PIPELINE_CONSUMER_TOPIC.to_string(), args.graph_config).await {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg = format!("Error subscribing to topic \"{PIPELINE_CONSUMER_TOPIC}\" {e}");
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
        
        //Expect transaction to start, stop if it doesn't
        match state.graph.start_txn().await {
            Ok(mut transaction) => {
                match message {
                    GitlabData::Pipelines((full_path, pipelines)) => {
                        let pipelines_data = pipelines
                        .iter()
                        .map(|pipeline| {
                            format!(
                                r#"{{ id: "{id}", 
                                active: "{active}",
                                created_at: "{created_at}",
                                sha: "{sha}", 
                                is_child: "{child}",
                                complete: "{complete}",
                                duration: "{duration}",
                                total_jobs: "{total_jobs}"
                                }}"#,
                                id = pipeline.id.0,
                                active = pipeline.active,
                                created_at = pipeline.created_at,
                                sha = pipeline.sha.clone().unwrap_or_default(),
                                child = pipeline.child,
                                complete = pipeline.complete,
                                duration = pipeline.duration.unwrap_or_default(),
                                total_jobs = pipeline.total_jobs
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",\n");
                
                        let cypher_query = format!(
                            "
                            UNWIND [{pipelines_data}] AS pipeline_data
                            MATCH (proj:GitlabProject {{ full_path: \"{full_path}\" }})
                            MERGE (p:GitlabPipeline {{ id: pipeline_data.id }})
                            SET p.id = pipeline_data.id,
                                p.status = pipeline_data.status,
                                p.created_at = pipeline_data.created_at,
                                p.sha = pipeline_data.sha,
                                p.duration = pipeline_data.duration,
                                p.complete = pipeline_data.complete,
                                p.total_jobs = pipeline_data.total_jobs
                                

                            MERGE (proj)-[:HAS_PIPELINE]->(p)
                            "
                        );
            
                    
                        debug!("Executing Cypher: {}", cypher_query);
                    
                        transaction.run(Query::new(cypher_query)).await?;
                    
                        if let Err(e) = transaction.commit().await {
                            error!("Failed to commit pipelines transaction: {:?}", e);
                            // Up to you if you want to stop the actor or recover
                        } else {
                            info!("Committed pipelines batch transaction to database");
                        }

                    }
                    _ => (),
                }        
            }
            Err(e) => myself.stop(Some(format!("{TRANSACTION_FAILED_ERROR}. {e}")))
        }

        Ok(())
    }
}
