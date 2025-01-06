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
use common::types::{MessageType, UserGroup, User, Runner, Project};
use futures_lite::StreamExt;
use lapin::{options::{BasicAckOptions, QueueBindOptions, BasicConsumeOptions}, Result, types::FieldTable};
use log::{debug, error, info};

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

    info!("[*] waiting to consume");
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
    info!("[*] Connected to neo4j");
    //begin consume loop

    while let Some(result) = consumer.next().await {
        match result {
            Ok(delivery) => {
                delivery
                .ack(BasicAckOptions::default())
                .await
                .expect("ack");
    
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
                MessageType::Groups(vec) => {
                    for g in vec as Vec<UserGroup> {
                        let query = format!("MERGE (n: GitlabUserGroup {{group_id: \"{}\", name: \"{}\", created_at: \"{}\" , visibility: \"{}\"}}) return n ", g.id, g.full_name, g.created_at, g.visibility);
                        if !helpers::helpers::run_query(&transaction, query).await {
                            continue
                        }
                    }
                },
                MessageType::GroupMembers(link) => {
                    for member in link.resource_vec as Vec<User> {
                        let query = format!("OPTIONAL MATCH (n:GitlabUser {{user_id: \"{}\"}}) WITH n WHERE n IS NULL CREATE (u:GitlabUser {{user_id: \"{}\", username: '{}', state: '{}'}} )",
                         member.id, member.id, member.username, member.state);
                         if !helpers::helpers::run_query(&transaction, query).await {
                            continue
                        }
                        let query = format!(" MATCH (g:GitlabUserGroup) WHERE g.group_id = '{}' with g MATCH (u:GitlabUser) WHERE u.user_id = '{}' with g, u MERGE (u)-[:inGroup]->(g) ", link.resource_id, member.id);
                        if !helpers::helpers::run_query(&transaction, query).await {
                            continue
                        }
                    }
                },
                MessageType::GroupRunners(link) => {
                    for runner in link.resource_vec as Vec<Runner> {
                        let query = format!("OPTIONAL MATCH (n:GitlabRunner {{runner_id: \"{}\"}}) WITH n WHERE n IS NULL CREATE (r:GitlabRunner {{runner_id: \"{}\", runner_type: '{}', ip_address: '{}'}} )",
                         runner.id, runner.id, runner.runner_type, runner.ip_address.unwrap_or_default());
                         if !helpers::helpers::run_query(&transaction, query).await {
                            continue
                        }
                        let query = format!("MATCH (n:GitlabUserGroup) WHERE n.group_id = '{}' with n MATCH (r:GitlabRunner) WHERE r.runner_id = '{}' with n, r MERGE (r)-[:inGroup]->(n)", link.resource_id, runner.id);
                        if !helpers::helpers::run_query(&transaction, query).await {
                            continue
                        }
                    }
                },
                MessageType::GroupProjects(link) => {
                    for project in link.resource_vec as Vec<Project> {
                        let query = format!("OPTIONAL MATCH (n:GitlabProject {{project_id: '{}'}}) WITH n WHERE n IS NULL CREATE (p:GitlabProject {{project_id: '{}', name: '{}', last_activity_at: '{}'}} )",
                         project.id, project.id, project.name, project.last_activity_at);
                         if !helpers::helpers::run_query(&transaction, query).await {
                            continue
                        }
                        let query = format!("MATCH (n:GitlabUserGroup) WHERE n.group_id = '{}' with n MATCH (p:GitlabProject) WHERE p.project_id = '{}' with n, p MERGE (p)-[:inGroup]->(n)", link.resource_id, project.id);
                        if !helpers::helpers::run_query(&transaction, query).await {
                            continue
                        }
                    }
                }
                _ => todo!()
            }
            //TODO: How to handle this error case? 
            match transaction.commit().await {
                Ok(_) => {
                    info!("[*] Transaction Committed")
                 },
                 Err(e) => error!("Error updating graph {}", e)
            }
    
            }
            Err(e) => {
                error!("Error getting message delivery! {}", e);
                continue;
            }
        };
       
    } //end consume loop

    Ok(())
}
