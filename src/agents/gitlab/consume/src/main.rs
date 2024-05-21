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

pub mod helpers;
use lapin::{options::*, types::FieldTable, Result};
use futures_lite::stream::StreamExt;
use gitlab_types::{User, MessageType, Project, UserGroup, Runner};
use neo4rs::{Query, Graph};
use helpers::helpers::get_neo_config;
use common::{connect_to_rabbitmq, GITLAB_EXCHANGE_STR};

#[tokio::main]
async fn main() -> Result<()> {

    //get mq connection
    let conn = connect_to_rabbitmq().await.unwrap();

    //create channels, exchange, 
    let consumer_channel = conn.create_channel().await?;

    //bind to queue
    //TODO: Get queue names from config, some shared value
    let queue_name = "gitlab";
    consumer_channel.queue_bind(queue_name, GITLAB_EXCHANGE_STR, "gitlab", QueueBindOptions::default(), FieldTable::default()).await?;

    println!("[*] waiting to consume");
    let mut consumer = consumer_channel
    .basic_consume(
        queue_name,
        "projects_consumer",
        BasicConsumeOptions::default(),
        FieldTable::default(),
    )
    .await?;

    //load neo config and connect to graph db TODO: get credentials securely
    let graph_conn = neo4rs::Graph::connect(get_neo_config()).await.unwrap();
    println!("[*] Connected to neo4j");
    //begin consume loop

    while let Some(delivery) = consumer.next().await {
        let delivery = delivery.expect("error in consumer");
        delivery
            .ack(BasicAckOptions::default())
            .await
            .expect("ack");

        // //deserialize json value
        let data_str = String::from_utf8(delivery.data).unwrap();
       // println!("received {}", data_str);
        
        let message : MessageType = serde_json::from_str(data_str.as_str()).unwrap();
        //Create query - return query, execut it
        let _ = update_graph(message, &graph_conn).await; 
    } //end consume looopa

    Ok(())
}

/*
    Creates Cypher queries based on the messagetype contents received from the resource observer
 */
async fn update_graph(message: MessageType, graph_conn: &Graph) -> Result<()> {
    //begin transaction
    let transaction = graph_conn.start_txn().await.unwrap();

    match message {
        MessageType::Users(user_vec) => {
            //build query from scratch
            for user in user_vec as Vec<User> {
                //create new nodes
                let query = format!("MERGE (n:GitlabUser {{username: \"{}\", user_id: \"{}\" , created_at: \"{}\" , state: \"{}\"}}) return n", user.username, user.id, user.created_at.unwrap_or("".to_string()), user.state);            
                //execute
                transaction.run(Query::new(query)).await.expect("Could not execute query on neo4j graph");
            }
        }
        MessageType::Projects(vec) => {
            for p in vec as Vec<Project> {
                let query = format!("MERGE (n: GitlabProject {{project_id: \"{}\", name: \"{}\", creator_id: \"{}\"}}) return n ", p.id, p.name, p.creator_id.unwrap());
                transaction.run(Query::new(query)).await.expect("Could not execute query on neo4j graph");
            }
        }
        MessageType::Groups(vec) => {
            for g in vec as Vec<UserGroup> {
                let query = format!("MERGE (n: GitlabUserGroup {{group_id: \"{}\", name: \"{}\", created_at: \"{}\" , visibility: \"{}\"}}) return n ", g.id, g.full_name, g.created_at, g.visibility);
                transaction.run(Query::new(query)).await.expect("Could not execute query on neo4j graph");
            }
        }
        MessageType::ProjectUsers(link) => {
            //add relationship for every user given
            for user in link.resource_vec as Vec<User>  {
                let query = format!("MATCH (p:GitlabProject) WHERE p.project_id = '{}' with p MATCH (u:GitlabUser) WHERE u.user_id = '{}' with p, u MERGE (u)-[:onProject]->(p)", link.resource_id, user.id);

                transaction.run(Query::new(query)).await.expect("could not execute query");
            }
        },
        MessageType::GroupMembers(link) => {
            for member in link.resource_vec as Vec<User> {
                let query = format!("MATCH (g:GitlabUserGroup) WHERE g.group_id = '{}' with g MATCH (u:GitlabUser) WHERE u.user_id = '{}' with g, u MERGE (u)-[:inGroup]->(g) ", link.resource_id, member.id);
                println!("{}", query);
                transaction.run(Query::new(query)).await.expect("could not execute query");
            }
        },
        MessageType::ProjectRunners(link) => {
            for runner in link.resource_vec as Vec<Runner> {
                let query = format!("MATCH (n:GitlabProject) WHERE n.project_id = '{}' with n MATCH (r:GitlabRunner) WHERE r.runner_id = '{}' with n, r MERGE (r)-[:onProject]->(n)", link.resource_id, runner.id);
                transaction.run(Query::new(query)).await.expect("could not execute query");
            }
        },
        MessageType::GroupRunners(link) => {
            for runner in link.resource_vec as Vec<Runner> {
                let query = format!("MATCH (n:GitlabUserGroup) WHERE n.group_id = '{}' with n MATCH (r:GitlabRunner) WHERE r.runner_id = '{}' with n, r MERGE (r)-[:inGroup]->(n)", link.resource_id, runner.id);
                transaction.run(Query::new(query)).await.expect("could not execute query");
            }
        },
        MessageType::Runners(vec) => {
            for runner in vec{
                let query = format!("MERGE (n:GitlabRunner {{runner_id: '{}', ip_address: '{}', name: '{}' , runner_type: '{}', status: '{}', is_shared: '{}'}}) return n", runner.id, runner.ip_address.unwrap_or_default(), runner.name.unwrap_or_default(), runner.runner_type, runner.status, runner.is_shared.unwrap_or(false));
                transaction.run(Query::new(query)).await.expect("could not execute query");
            }
        },
        MessageType::Jobs(vec) => {
            for job in vec {
                let q =  format!("MERGE (j:GitlabJob {{ job_id: '{}', name: '{}', stage: '{}', status: '{}', project_id: '{}', user_id: '{}', pipeline_id: '{}',created_at: '{}', finished_at: '{}'}}) return j",
                     job.id, job.name, job.stage, job.status, job.project["id"],job.user.unwrap().id, job.pipeline.id, job.created_at, job.finished_at.unwrap_or("".to_string()));
                     println!("{}",q);
                     transaction.run(Query::new(q)).await.expect("Could not execute query");
            }
        },
        MessageType::Pipelines(vec) => {
            for pipeline in vec {
                //create pipeline
                let q =  format!("MERGE (j:GitlabPipeline {{ pipeline_id: '{}', project_id: '{}', status: '{}', created_at: '{}', updated_at: '{}'}}) return j", pipeline.id, pipeline.project_id.unwrap_or_default(), pipeline.status, pipeline.created_at, pipeline.updated_at);
                println!("{}",q);
                transaction.run(Query::new(q)).await.expect("Could not execute query");
                //create releationship to project
                let q = format!("MATCH (p:GitlabProject) where p.project_id = '{}' with p MATCH (q:GitlabPipeline) where q.pipeline_id = '{}' with p,q MERGE (q)-[:onProject]->(p)", pipeline.project_id.unwrap(), pipeline.id);
                println!("{}",q);
                transaction.run(Query::new(q)).await.expect("could not execute query");
            }
        },
        MessageType::PipelineJobs(link) => {
            for job in link.resource_vec {
                let q = format!("MATCH (p:GitlabPipeline) WHERE p.pipeline_id = '{}' with p MATCH (j:GitlabJob) WHERE j.job_id = '{}' with p,j MERGE (j)-[:inPipeline]->(p)", link.resource_id, job.id);
                transaction.run(Query::new(q)).await.expect("Could not execute query");
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
   //commit
   match transaction.commit().await {
        Ok(_) => {
            Ok(())
        },
        Err(e) => panic!("Error updating graph {}", e)
   }
}
