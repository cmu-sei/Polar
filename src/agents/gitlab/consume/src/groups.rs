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
use common::{types::{GitlabData, Project, Runner, User, UserGroup}, GROUPS_QUEUE_NAME};
use crate::{get_neo_config, run_query, subscribe_to_topic, GitlabConsumerArgs, GitlabConsumerState};
use common::{connect_to_rabbitmq, GITLAB_EXCHANGE_STR, USERS_QUEUE_NAME, USERS_ROUTING_KEY};
use tracing::{debug, error, info};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};


pub struct GitlabGroupConsumer;

#[async_trait]
impl Actor for GitlabGroupConsumer {
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
        match subscribe_to_topic(args.registration_id, GROUPS_QUEUE_NAME.to_string()).await {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg = format!("Error subscribing to topic \"{GROUPS_QUEUE_NAME}\" {e}");
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
        _: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {

        // match message {
        //     ConsumerMessage::GitlabData(data_type) => {
        //         match state.graph.start_txn().await {
        //             Ok(transaction) => {
        //                 match data_type {
        //                     GitlabData::Groups(vec) => {
        //                         for g in vec as Vec<UserGroup> {
        //                             let query = format!("MERGE (n: GitlabGroupGroup {{group_id: \"{}\", name: \"{}\", created_at: \"{}\" , visibility: \"{}\"}}) return n ", g.id, g.full_name, g.created_at, g.visibility);
        //                             run_query(&transaction, query).await;
        //                         }
        //                     },
        //                     GitlabData::GroupMembers(link) => {
        //                         for member in link.resource_vec as Vec<User> {
        //                             let query = format!("OPTIONAL MATCH (n:GitlabGroup {{user_id: \"{}\"}}) WITH n WHERE n IS NULL CREATE (u:GitlabGroup {{user_id: \"{}\", username: '{}', state: '{}'}} )",
        //                             member.id, member.id, member.username, member.state);
        //                             run_query(&transaction, query).await;
        //                             let query = format!(" MATCH (g:GitlabGroupGroup) WHERE g.group_id = '{}' with g MATCH (u:GitlabGroup) WHERE u.user_id = '{}' with g, u MERGE (u)-[:inGroup]->(g) ", link.resource_id, member.id);
        //                             run_query(&transaction, query).await;
        //                         }
        //                     },
        //                     GitlabData::GroupRunners(link) => {
        //                         for runner in link.resource_vec as Vec<Runner> {
        //                             let query = format!("OPTIONAL MATCH (n:GitlabRunner {{runner_id: \"{}\"}}) WITH n WHERE n IS NULL CREATE (r:GitlabRunner {{runner_id: \"{}\", runner_type: '{}', ip_address: '{}'}} )",
        //                             runner.id, runner.id, runner.runner_type, runner.ip_address.unwrap_or_default());
        //                             run_query(&transaction, query).await;
        //                             let query = format!("MATCH (n:GitlabGroupGroup) WHERE n.group_id = '{}' with n MATCH (r:GitlabRunner) WHERE r.runner_id = '{}' with n, r MERGE (r)-[:inGroup]->(n)", link.resource_id, runner.id);
        //                             run_query(&transaction, query).await;
        //                         }
        //                     },
        //                     GitlabData::GroupProjects(link) => {
        //                         for project in link.resource_vec as Vec<Project> {
        //                             let query = format!("OPTIONAL MATCH (n:GitlabProject {{project_id: '{}'}}) WITH n WHERE n IS NULL CREATE (p:GitlabProject {{project_id: '{}', name: '{}', last_activity_at: '{}'}} )",
        //                             project.id, project.id, project.name, project.last_activity_at);
        //                             run_query(&transaction, query).await;
        //                             let query = format!("MATCH (n:GitlabGroupGroup) WHERE n.group_id = '{}' with n MATCH (p:GitlabProject) WHERE p.project_id = '{}' with n, p MERGE (p)-[:inGroup]->(n)", link.resource_id, project.id);
        //                             run_query(&transaction, query).await;
        //                         }
        //                     }
        //                     _ => todo!()
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
//     consumer_channel.queue_bind(GROUPS_QUEUE_NAME, GITLAB_EXCHANGE_STR, GROUPS_ROUTING_KEY, QueueBindOptions::default(), FieldTable::default()).await?;

//     info!("[*] waiting to consume");
//     let mut consumer = consumer_channel
//     .basic_consume(
//         GROUPS_QUEUE_NAME,
//         "groups_consumer",
//         BasicConsumeOptions::default(),
//         FieldTable::default(),
//     )
//     .await?;

//     //load neo config and connect to graph db TODO: get credentials securely
//     let graph_conn = neo4rs::Graph::connect(get_neo_config()).await.unwrap();
//     info!("[*] Connected to neo4j");
//     //begin consume loop

//     while let Some(result) = consumer.next().await {
//         match result {
//             Ok(delivery) => {
//                 delivery
//                 .ack(BasicAckOptions::default())
//                 .await
//                 .expect("ack");
    
//             //TODO: What else can be done on these error cases? Log message data?
//             let message : GitlabData = match serde_json::from_slice(delivery.data.as_slice()) {
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
    
//             match message {
//                 GitlabData::Groups(vec) => {
//                     for g in vec as Vec<UserGroup> {
//                         let query = format!("MERGE (n: GitlabGroupGroup {{group_id: \"{}\", name: \"{}\", created_at: \"{}\" , visibility: \"{}\"}}) return n ", g.id, g.full_name, g.created_at, g.visibility);
//                         if !lib::helpers::run_query(&transaction, query).await {
//                             continue
//                         }
//                     }
//                 },
//                 GitlabData::GroupMembers(link) => {
//                     for member in link.resource_vec as Vec<User> {
//                         let query = format!("OPTIONAL MATCH (n:GitlabGroup {{user_id: \"{}\"}}) WITH n WHERE n IS NULL CREATE (u:GitlabGroup {{user_id: \"{}\", username: '{}', state: '{}'}} )",
//                          member.id, member.id, member.username, member.state);
//                          if !lib::helpers::run_query(&transaction, query).await {
//                             continue
//                         }
//                         let query = format!(" MATCH (g:GitlabGroupGroup) WHERE g.group_id = '{}' with g MATCH (u:GitlabGroup) WHERE u.user_id = '{}' with g, u MERGE (u)-[:inGroup]->(g) ", link.resource_id, member.id);
//                         if !lib::helpers::run_query(&transaction, query).await {
//                             continue
//                         }
//                     }
//                 },
//                 GitlabData::GroupRunners(link) => {
//                     for runner in link.resource_vec as Vec<Runner> {
//                         let query = format!("OPTIONAL MATCH (n:GitlabRunner {{runner_id: \"{}\"}}) WITH n WHERE n IS NULL CREATE (r:GitlabRunner {{runner_id: \"{}\", runner_type: '{}', ip_address: '{}'}} )",
//                          runner.id, runner.id, runner.runner_type, runner.ip_address.unwrap_or_default());
//                          if !lib::helpers::run_query(&transaction, query).await {
//                             continue
//                         }
//                         let query = format!("MATCH (n:GitlabGroupGroup) WHERE n.group_id = '{}' with n MATCH (r:GitlabRunner) WHERE r.runner_id = '{}' with n, r MERGE (r)-[:inGroup]->(n)", link.resource_id, runner.id);
//                         if !lib::helpers::run_query(&transaction, query).await {
//                             continue
//                         }
//                     }
//                 },
//                 GitlabData::GroupProjects(link) => {
//                     for project in link.resource_vec as Vec<Project> {
//                         let query = format!("OPTIONAL MATCH (n:GitlabProject {{project_id: '{}'}}) WITH n WHERE n IS NULL CREATE (p:GitlabProject {{project_id: '{}', name: '{}', last_activity_at: '{}'}} )",
//                          project.id, project.id, project.name, project.last_activity_at);
//                          if !lib::helpers::run_query(&transaction, query).await {
//                             continue
//                         }
//                         let query = format!("MATCH (n:GitlabGroupGroup) WHERE n.group_id = '{}' with n MATCH (p:GitlabProject) WHERE p.project_id = '{}' with n, p MERGE (p)-[:inGroup]->(n)", link.resource_id, project.id);
//                         if !lib::helpers::run_query(&transaction, query).await {
//                             continue
//                         }
//                     }
//                 }
//                 _ => todo!()
//             }
//             //TODO: How to handle this error case? 
//             match transaction.commit().await {
//                 Ok(_) => {
//                     info!("[*] Transaction Committed")
//                  },
//                  Err(e) => error!("Error updating graph {}", e)
//             }
    
//             }
//             Err(e) => {
//                 error!("Error getting message delivery! {}", e);
//                 continue;
//             }
//         };
       
//     } //end consume loop

//     Ok(())
// }
