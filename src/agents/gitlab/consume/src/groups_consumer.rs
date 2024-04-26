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

use common::{GITLAB_EXCHANGE_STR, connect_to_rabbitmq, GROUPS_ROUTING_KEY, GROUPS_QUEUE_NAME};
use futures_lite::StreamExt;
use gitlab_types::{MessageType, UserGroup, User, Runner, Project};
use lapin::{options::{BasicAckOptions, QueueBindOptions, BasicConsumeOptions}, Result, types::FieldTable};
use neo4rs::Query;
// use std::fmt::format;

use crate::helpers::helpers::get_neo_config;
mod helpers;
#[tokio::main]
async fn main() -> Result<()> {
    //get mq connection
    let conn = connect_to_rabbitmq().await.unwrap();

    //create channels, exchange, 
    let consumer_channel = conn.create_channel().await?;

    //bind to queue
    consumer_channel.queue_bind(GROUPS_QUEUE_NAME, GITLAB_EXCHANGE_STR, GROUPS_ROUTING_KEY, QueueBindOptions::default(), FieldTable::default()).await?;

    println!("[*] waiting to consume");
    let mut consumer = consumer_channel
    .basic_consume(
        GROUPS_QUEUE_NAME,
        "groups_consumer",
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

        let transaction = graph_conn.start_txn().await.unwrap();

        match message {
            MessageType::Groups(vec) => {
                for g in vec as Vec<UserGroup> {
                    let query = format!("MERGE (n: GitlabUserGroup {{group_id: \"{}\", name: \"{}\", created_at: \"{}\" , visibility: \"{}\"}}) return n ", g.id, g.full_name, g.created_at, g.visibility);
                    transaction.run(Query::new(query)).await.expect("Could not execute query on neo4j graph");
                }
            },
            MessageType::GroupMembers(link) => {
                for member in link.resource_vec as Vec<User> {
                    let q = format!("OPTIONAL MATCH (n:GitlabUser {{user_id: \"{}\"}}) WITH n WHERE n IS NULL CREATE (u:GitlabUser {{user_id: \"{}\", username: '{}', state: '{}'}} )",
                     member.id, member.id, member.username, member.state);
                    transaction.run(Query::new(q)).await.expect("could not execute query");
                    let query = format!(" MATCH (g:GitlabUserGroup) WHERE g.group_id = '{}' with g MATCH (u:GitlabUser) WHERE u.user_id = '{}' with g, u MERGE (u)-[:inGroup]->(g) ", link.resource_id, member.id);
                    println!("{}", query);
                    transaction.run(Query::new(query)).await.expect("could not execute query");
                }
            },
            MessageType::GroupRunners(link) => {
                println!("[**]GITLAB GROUP RUNNERS RECEIVED[**]");
                for runner in link.resource_vec as Vec<Runner> {
                    let q = format!("OPTIONAL MATCH (n:GitlabRunner {{runner_id: \"{}\"}}) WITH n WHERE n IS NULL CREATE (r:GitlabRunner {{runner_id: \"{}\", runner_type: '{}', ip_address: '{}'}} )",
                     runner.id, runner.id, runner.runner_type, runner.ip_address);
                    transaction.run(Query::new(q)).await.expect("could not execute query");
                    let query = format!("MATCH (n:GitlabUserGroup) WHERE n.group_id = '{}' with n MATCH (r:GitlabRunner) WHERE r.runner_id = '{}' with n, r MERGE (r)-[:inGroup]->(n)", link.resource_id, runner.id);
                    println!("{}", query);
                    transaction.run(Query::new(query)).await.expect("could not execute query");
                }
            },
            MessageType::GroupProjects(link) => {
                println!("[**]GITLAB GROUP PROJECTS RECEIVED[**]");
                for project in link.resource_vec as Vec<Project> {
                    let q = format!("OPTIONAL MATCH (n:GitlabProject {{project_id: '{}'}}) WITH n WHERE n IS NULL CREATE (p:GitlabProject {{project_id: '{}', name: '{}', last_activity_at: '{}'}} )",
                     project.id, project.id, project.name, project.last_activity_at);
                    transaction.run(Query::new(q)).await.expect("could not execute query");
                    let query = format!("MATCH (n:GitlabUserGroup) WHERE n.group_id = '{}' with n MATCH (p:GitlabProject) WHERE p.project_id = '{}' with n, p MERGE (p)-[:inGroup]->(n)", link.resource_id, project.id);
                    println!("{}", query);
                    transaction.run(Query::new(query.clone())).await.expect(format!("Could not execute query: {}", query).as_str());
                }
            }
            _ => todo!()
        }
        match transaction.commit().await {
            Ok(_) => {
                println!("[*] Transaction Committed")
             },
             Err(e) => panic!("Error updating graph {}", e)
        }

    } //end consume loop

    Ok(())
}
