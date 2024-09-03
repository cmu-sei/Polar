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
use gitlab_types::{MessageType, User, Runner, Project};
use helpers::helpers::get_neo_config;
use common::{connect_to_rabbitmq, GITLAB_EXCHANGE_STR, PROJECTS_ROUTING_KEY, PROJECTS_QUEUE_NAME};
use lapin::{options::*, types::FieldTable, Result};
use neo4rs::Query;
use log::{info,error,debug};

mod helpers;

#[tokio::main]
async fn main() -> Result<()> {
    
    env_logger::init();

    //get mq connection
    let conn = connect_to_rabbitmq().await.unwrap();

    //create channels, exchange, 
    let consumer_channel = conn.create_channel().await?;

    //bind to queue
    //TODO: Get queue names from config, some shared value
    consumer_channel.queue_bind(PROJECTS_QUEUE_NAME, GITLAB_EXCHANGE_STR, PROJECTS_ROUTING_KEY, QueueBindOptions::default(), FieldTable::default()).await?;

    let mut consumer = consumer_channel
    .basic_consume(
        PROJECTS_QUEUE_NAME,
        "projects_consumer",
        BasicConsumeOptions::default(),
        FieldTable::default(),
    )
    .await?;
    info!("Consumer is waiting...");

    //load neo config and connect to graph db TODO: get credentials securely
    let graph_conn = neo4rs::Graph::connect(get_neo_config()).await.unwrap();
    info!("Connected to neo4j");

    while let Some(delivery) = consumer.next().await {
        let delivery = delivery.expect("error in consumer");
        delivery
            .ack(BasicAckOptions::default())
            .await
            .expect("ack");

        // Deserialize json value.
        let data_str = String::from_utf8(delivery.data).unwrap();
        
        let message : MessageType = serde_json::from_str(data_str.as_str()).unwrap();
        // Create query - return query, execute it.
        let transaction = graph_conn.start_txn().await.unwrap();

        match message {
            MessageType::Projects(vec) => {
                for p in vec as Vec<Project> {
                    let query = format!("MERGE (n: GitlabProject {{project_id: \"{}\", name: \"{}\", creator_id: \"{}\"}}) return n ", p.id, p.name, p.creator_id.unwrap());
                    transaction.run(Query::new(query)).await.expect("Could not execute query on neo4j graph");
                }
            },
            MessageType::ProjectUsers(link) => {
                //add relationship for every user given
                for user in link.resource_vec as Vec<User>  {
                    let query = format!("MATCH (p:GitlabProject) WHERE p.project_id = '{}' with p MATCH (u:GitlabUser) WHERE u.user_id = '{}' with p, u MERGE (u)-[:onProject]->(p)", link.resource_id, user.id);
    
                    transaction.run(Query::new(query)).await.expect("could not execute query");
                }
            },
            MessageType::ProjectRunners(link) => {
                for runner in link.resource_vec as Vec<Runner> {
                    let query = format!("MATCH (n:GitlabProject) WHERE n.project_id = '{}' with n MATCH (r:GitlabRunner) WHERE r.runner_id = '{}' with n, r MERGE (r)-[:onProject]->(n)", link.resource_id, runner.id);
                    transaction.run(Query::new(query)).await.expect("could not execute query");
                }
            },
            MessageType::Pipelines(vec) => {
                for pipeline in vec {
                    //create pipeline
                    let q =  format!("MERGE (j:GitlabPipeline {{ pipeline_id: '{}', project_id: '{}', status: '{}', created_at: '{}', updated_at: '{}'}}) return j", pipeline.id, pipeline.project_id.unwrap_or_default(), pipeline.status, pipeline.created_at, pipeline.updated_at);
                    debug!("{}",q);
                    transaction.run(Query::new(q)).await.expect("Could not execute query");
                    //create relationship to project
                    let q = format!("MATCH (p:GitlabProject) where p.project_id = '{}' with p MATCH (q:GitlabPipeline) where q.pipeline_id = '{}' with p,q MERGE (q)-[:onProject]->(p)", pipeline.project_id.unwrap(), pipeline.id);
                    debug!("{}",q);
                    transaction.run(Query::new(q)).await.expect("could not execute query");
                }
            },
            MessageType::PipelineJobs(link) => {
                for job in link.resource_vec {
                    let q = format!("MATCH (p:GitlabPipeline) WHERE p.pipeline_id = '{}' with p MATCH (j:GitlabJob) WHERE j.job_id = '{}' with p,j MERGE (j)-[:inPipeline]->(p)", link.resource_id, job.id);
                    transaction.run(Query::new(q)).await.expect("Could not execute query");
                }
            },
            _ => {
                todo!()
            }
        }
        match transaction.commit().await {
             Ok(_) => {
                debug!("Transaction Committed")
             },
             Err(e) => panic!("Error updating graph {}", e)
        }        
    } //end consume loop

    Ok(())

}
