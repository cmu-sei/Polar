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

use futures_lite::StreamExt;
use gitlab_types::MessageType;
use helpers::helpers::{get_neo_config, run_query};
use common::{connect_to_rabbitmq, GITLAB_EXCHANGE_STR, RUNNERS_ROUTING_KEY, RUNNERS_QUEUE_NAME};
use lapin::{options::*, types::FieldTable, Result};
use log::{debug, error, info};
mod helpers;

#[tokio::main]
async fn main() -> Result<()> {
    
    //get mq connection
    let conn = connect_to_rabbitmq().await.unwrap();

    //create channels, exchange, 
    let consumer_channel = conn.create_channel().await?;

    //bind to queue
    //TODO: Get queue names from config, some shared value
    consumer_channel.queue_bind(RUNNERS_QUEUE_NAME, GITLAB_EXCHANGE_STR, RUNNERS_ROUTING_KEY, QueueBindOptions::default(), FieldTable::default()).await?;

    let mut consumer = consumer_channel
    .basic_consume(
        RUNNERS_QUEUE_NAME,
        "runner_consumer",
        BasicConsumeOptions::default(),
        FieldTable::default(),
    )
    .await?;
    info!("[*] waiting to consume");

    //load neo config and connect to graph db TODO: get credentials securely
    let graph_conn = neo4rs::Graph::connect(get_neo_config()).await.unwrap();
    info!("[*] Connected to neo4j");

    while let Some(result) = consumer.next().await {
        match result {
            Ok(delivery) => {
                delivery
                    .ack(BasicAckOptions::default())
                    .await
                    .expect("ack");
                // //deserialize json value
            // //deserialize json value
            //TODO: What else can be done on these error cases? Log message data?
            let message : MessageType = match serde_json::from_slice(delivery.data.as_slice()) {
                Ok(msg) => msg,
                Err(e) => {
                    error!("Could not deserialize message! {}", e);
                    continue
                }
            };
            
            debug!("{:?}", message);
            
            let transaction = match graph_conn.start_txn().await {
                Ok(t) => t,
                Err(e) => {
                    error!("Could not open transaction with graph! {}", e);
                    continue
                }
            };
            
                match message {
                    MessageType::Runners(vec) => {
                        for runner in vec{
                            let query = format!("MERGE (n:GitlabRunner {{runner_id: '{}', ip_address: '{}', name: '{}' , runner_type: '{}', status: '{}', is_shared: '{}'}}) return n",
                            runner.id, runner.ip_address.unwrap_or_default(), runner.name.unwrap_or_default(), 
                            runner.runner_type, runner.status, runner.is_shared.unwrap_or(false));
                            if !run_query(&transaction, query).await {
                                continue;
                            }
                        }
                    },
                    MessageType::RunnerJob((runner_id, job)) =>{
                        let q = format!("MATCH (r:GitlabRunner) where r.runner_id = '{}' with r MATCH (j:GitlabJob) where j.job_id = '{}' with j,r MERGE (r)-[:hasJob]->(j)", runner_id, job.id);
                        if !run_query(&transaction, q).await {
                            continue;
                        }
                        let q = format!("MATCH (p:GitlabPipeline) WHERE p.pipeline_id = '{}' with p MATCH (j:GitlabJob) WHERE j.job_id = '{}' with p,j MERGE (j)-[:inPipeline]->(p)", job.pipeline.id, job.id);
                        if !run_query(&transaction, q).await {
                            continue;
                        }
                    },
                    _ => {
                        todo!()
                    }
                }
                match transaction.commit().await {
                    Ok(_) => {
                        info!("[*] Transaction Committed")
                    },
                    Err(e) => panic!("Error updating graph {}", e)
                }        
            }Err(e) => {
                error!("Error getting message Delivery!{}", e );
                continue
            }
        }           
    } //end consume looopa

    Ok(())

}
