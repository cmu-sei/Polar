// Polar
// Copyright 2023 Carnegie Mellon University.
// NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.
// [DISTRIBUTION STATEMENT D] Distribution authorized to the Department of Defense and U.S. DoD contractors only (materials contain software documentation) (determination date: 2022-05-20). Other requests shall be referred to Defense Threat Reduction Agency.
// Notice to DoD Subcontractors:  This document may contain Covered Defense Information (CDI).  Handling of this information is subject to the controls identified in DFARS 252.204-7012 – SAFEGUARDING COVERED DEFENSE INFORMATION AND CYBER INCIDENT REPORTING
// Carnegie Mellon® is registered in the U.S. Patent and Trademark Office by Carnegie Mellon University.
// This Software includes and/or makes use of Third-Party Software subject to its own license, see license.txt file for more information. 
// DM23-0821
// 
use futures_lite::StreamExt;
use gitlab_types::{MessageType};
use helpers::helpers::get_neo_config;
use common::{connect_to_rabbitmq, GITLAB_EXCHANGE_STR, RUNNERS_ROUTING_KEY, RUNNERS_QUEUE_NAME};
use lapin::{options::*, types::FieldTable, Result};
use neo4rs::Query;
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
    println!("[*] waiting to consume");

    //load neo config and connect to graph db TODO: get credentials securely
    let graph_conn = neo4rs::Graph::connect(get_neo_config()).await.unwrap();
    println!("[*] Connected to neo4j");

    while let Some(delivery) = consumer.next().await {
        let delivery = delivery.expect("error in consumer");
        delivery
            .ack(BasicAckOptions::default())
            .await
            .expect("ack");

        // //deserialize json value
        let data_str = String::from_utf8(delivery.data).unwrap();
        
        let message : MessageType = serde_json::from_str(data_str.as_str()).unwrap();
        //Create query - return query, execut it
        let transaction = graph_conn.start_txn().await.unwrap();

        match message {
            MessageType::Runners(vec) => {
                for runner in vec{
                    let query = format!("MERGE (n:GitlabRunner {{runner_id: '{}', ip_address: '{}', name: '{}' , runner_type: '{}', status: '{}', is_shared: '{}'}}) return n",
                     runner.id, runner.ip_address, runner.name.unwrap_or_default(), 
                     runner.runner_type, runner.status, runner.is_shared.unwrap_or(false));

                    transaction.run(Query::new(query)).await.expect("could not execute query");
                }
            },
            MessageType::RunnerJob((runner_id, job)) =>{
                let q = format!("MATCH (r:GitlabRunner) where r.runner_id = '{}' with r MATCH (j:GitlabJob) where j.job_id = '{}' with j,r MERGE (r)-[:hasJob]->(j)", runner_id, job.id);
                transaction.run(Query::new(q)).await.expect("Could not execute query");
                let q = format!("MATCH (p:GitlabPipeline) WHERE p.pipeline_id = '{}' with p MATCH (j:GitlabJob) WHERE j.job_id = '{}' with p,j MERGE (j)-[:inPipeline]->(p)", job.pipeline.id, job.id);
                transaction.run(Query::new(q)).await.expect("Could not execute query");
            },
            _ => {
                todo!()
            }
        }
        match transaction.commit().await {
             Ok(_) => {
                println!("[*] Transaction Committed")
             },
             Err(e) => panic!("Error updating graph {}", e)
        }        
    } //end consume looopa

    Ok(())

}
