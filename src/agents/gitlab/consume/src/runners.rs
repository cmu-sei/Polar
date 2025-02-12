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

use common::types::GitlabData;
use neo4rs::Query;
use crate::{subscribe_to_topic, GitlabConsumerArgs, GitlabConsumerState};
use common::{RUNNERS_CONSUMER_TOPIC};

use tracing::{debug, error, info};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};


pub struct GitlabRunnerConsumer;

#[async_trait]
impl Actor for GitlabRunnerConsumer {
    type Msg = GitlabData;
    type State = GitlabConsumerState;
    type Arguments = GitlabConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabConsumerArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");
        //subscribe to topic
        match subscribe_to_topic(args.registration_id, RUNNERS_CONSUMER_TOPIC.to_string()).await {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg = format!("Error subscribing to topic \"{RUNNERS_CONSUMER_TOPIC}\" {e}");
                Err(ActorProcessingErr::from(err_msg))
            }
        }
    }

    async fn post_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
        info!("[*] waiting to consume");
        
        Ok(())
    }
    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {

        match message {
            GitlabData::Runners(runners) => {
                                //TODO: Expect transaction to start, panic if it doesn't
                match state.graph.start_txn().await {
                    Ok(transaction)  => {
                        
                        let mut_cypher_query = String::new();

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
                            "
                            UNWIND [{runner_data}] AS runner_data
                            MERGE (runner:GitlabRunner {{ runner_id: runner_data.runner_id }})
                            SET runner.paused = runner_data.paused,
                                runner.runner_type = runner_data.runner_type,
                                runner.status = runner_data.status,
                                runner.access_level = runner_data.access_level, 
                                runner.run_untagged = runner_data.run_untagged,
                                runner.tag_list = runner_data.tag_list 
                            "
                        );
                        debug!(cypher_query);
                        transaction.run(Query::new(cypher_query)).await.expect("Expected to run query."); 
                        if let Err(e) = transaction.commit().await {
                            let err_msg = format!("Error committing transaction to graph: {e}");
                            error!("{err_msg}");
                        }
                        info!("Committed transaction to database");
                    }
                    Err(e) => {
                        error!("Could not open transaction with graph! {e}");
                        todo!("What to do when we can't access the graph")
                    }
                } 
            }
            _ => ()
        }
        //TODO: Implement message type for consumers to handle new messages
        // match message {
        //     ConsumerMessage::GitlabData(data_type) => {
        //         match state.graph.start_txn().await {
        //             Ok(transaction) => {
        //                 match data_type {

        //                     GitlabData::RunnerJob((runner_id, job)) =>{
        //                         let query = format!("MATCH (r:GitlabRunner) where r.runner_id = '{}' with r MATCH (j:GitlabJob) where j.job_id = '{}' with j,r MERGE (r)-[:hasJob]->(j)", runner_id, job.id);
        //                         run_query(&transaction, query).await;
        //                         let query = format!("MATCH (p:GitlabPipeline) WHERE p.pipeline_id = '{}' with p MATCH (j:GitlabJob) WHERE j.job_id = '{}' with p,j MERGE (j)-[:inPipeline]->(p)", job.pipeline.id, job.id);
        //                         run_query(&transaction, query).await;
        //                     },
        //                     _ => { todo!() }
        //                 }
        //                 if let Err(e) = transaction.commit().await {
        //                     let err_msg = format!("Error committing transaction to graph: {e}");
        //                     error!("{err_msg}");
        //                     todo!("What to do if we fail to commit queries?");
        //                 }
        //             }
        //             Err(e) => todo!()
        //         }
                
        //     },

        //     _ => todo!("Gitlab consumer shouldn't get anything but gitlab data")
        // }
        Ok(())
    }
}

// #[tokio::main]
// async fn main() -> Result<()> {
    
//     //get mq connection
//     let conn = connect_to_rabbitmq().await.unwrap();

//     //create channels, exchange, 
//     let consumer_channel = conn.create_channel().await?;

//     //bind to queue
//     //TODO: Get queue names from config, some shared value
//     consumer_channel.queue_bind(RUNNERS_QUEUE_NAME, GITLAB_EXCHANGE_STR, RUNNERS_ROUTING_KEY, QueueBindOptions::default(), FieldTable::default()).await?;

//     let mut consumer = consumer_channel
//     .basic_consume(
//         RUNNERS_QUEUE_NAME,
//         "runner_consumer",
//         BasicConsumeOptions::default(),
//         FieldTable::default(),
//     )
//     .await?;
//     info!("[*] waiting to consume");

//     //load neo config and connect to graph db TODO: get credentials securely
//     let graph_conn = neo4rs::Graph::connect(get_neo_config()).await.unwrap();
//     info!("[*] Connected to neo4j");

//     while let Some(result) = consumer.next().await {
//         match result {
//             Ok(delivery) => {
//                 delivery
//                     .ack(BasicAckOptions::default())
//                     .await
//                     .expect("ack");
//                 // //deserialize json value
//             // //deserialize json value
//             //TODO: What else can be done on these error cases? Log message data?
//             let message : gitlabDa = match serde_json::from_slice(delivery.data.as_slice()) {
//                 Ok(msg) => msg,
//                 Err(e) => {
//                     error!("Could not deserialize message! {}", e);
//                     continue
//                 }
//             };
            
//             debug!("{:?}", message);
            
//             let transaction = match graph_conn.start_txn().await {
//                 Ok(t) => t,
//                 Err(e) => {
//                     error!("Could not open transaction with graph! {}", e);
//                     continue
//                 }
//             };
            
//                 match message {
//                     gitlabDa::Runners(vec) => {
//                         for runner in vec{
//                             let query = format!("MERGE (n:GitlabRunner {{runner_id: '{}', ip_address: '{}', name: '{}' , runner_type: '{}', status: '{}', is_shared: '{}'}}) return n",
//                             runner.id, runner.ip_address.unwrap_or_default(), runner.name.unwrap_or_default(), 
//                             runner.runner_type, runner.status, runner.is_shared.unwrap_or(false));
//                             if !run_query(&transaction, query).await {
//                                 continue;
//                             }
//                         }
//                     },
//                     gitlabDa::RunnerJob((runner_id, job)) =>{
//                         let q = format!("MATCH (r:GitlabRunner) where r.runner_id = '{}' with r MATCH (j:GitlabJob) where j.job_id = '{}' with j,r MERGE (r)-[:hasJob]->(j)", runner_id, job.id);
//                         if !run_query(&transaction, q).await {
//                             continue;
//                         }
//                         let q = format!("MATCH (p:GitlabPipeline) WHERE p.pipeline_id = '{}' with p MATCH (j:GitlabJob) WHERE j.job_id = '{}' with p,j MERGE (j)-[:inPipeline]->(p)", job.pipeline.id, job.id);
//                         if !run_query(&transaction, q).await {
//                             continue;
//                         }
//                     },
//                     _ => {
//                         todo!()
//                     }
//                 }
//                 match transaction.commit().await {
//                     Ok(_) => {
//                         info!("[*] Transaction Committed")
//                     },
//                     Err(e) => panic!("Error updating graph {}", e)
//                 }        
//             }Err(e) => {
//                 error!("Error getting message Delivery!{}", e );
//                 continue
//             }
//         }           
//     } //end consume looopa

//     Ok(())

// }
