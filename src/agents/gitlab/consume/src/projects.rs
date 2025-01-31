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


use common::PROJECTS_CONSUMER_TOPIC;
use common::types::{GitlabData};
use neo4rs::Query;
use tracing::{debug, error, info};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use crate::{subscribe_to_topic, GitlabConsumerArgs, GitlabConsumerState, BROKER_CLIENT_NAME};

pub struct GitlabProjectConsumer;

#[async_trait]
impl Actor for GitlabProjectConsumer {
    type Msg = GitlabData;
    type State = GitlabConsumerState;
    type Arguments = GitlabConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabConsumerArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");
        match subscribe_to_topic(args.registration_id, PROJECTS_CONSUMER_TOPIC.to_string()).await {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg = format!("Error subscribing to topic \"{PROJECTS_CONSUMER_TOPIC}\" {e}");
                Err(ActorProcessingErr::from(err_msg))
            }
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
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
            GitlabData::Projects(vec) => {
                let transaction = state.graph.start_txn().await.expect("Expected to start a transaction with the graph.");
                for p in vec {
                    let query = format!(
                    r#"
                        MERGE (project:GitlabProject {{ 
                            project_id: "{id}"
                        }})
                        SET project.name = "{name}",
                            project.full_path = "{full_path}",
                            project.created_at = "{created_at}",
                            project.last_activity_at = "{last_activity_at}"

                        MERGE (group:GitlabGroup {{ group_id: "{group_id}" }})

                        MERGE (project)-[:inGroup]->(group);
                    "#, 
                    id = p.id,
                    name = p.name,
                    full_path = p.full_path,
                    created_at = p.created_at.unwrap_or_default(),
                    last_activity_at = p.last_activity_at.unwrap_or_default(),
                    group_id = p.group.map_or_else(|| {String::default()}, |group| { group.id.to_string() })
                    
                    );

                    debug!(query);

                    transaction.run(Query::new(query)).await.expect("Expected to run query on transaction.");
                }
                transaction.commit().await.expect("Expected to commit transaction");
            }
            _ => todo!()
        }
        
        //TODO: Implement message type for consumers to handle new messages
        // match message {
        //     ConsumerMessage::GitlabData(data_type) => {
        //         match state.graph.start_txn().await {
        //             Ok(transaction) => {
        //                 match data_type {
        //                     GitlabData::Projects(vec) => {
        //                         for p in vec as Vec<Project> {
        //                             let query = format!("MERGE (n: GitlabProject {{project_id: \"{}\", name: \"{}\", creator_id: \"{}\"}}) return n ", p.id, p.name, p.creator_id.unwrap());
        //                             run_query(&transaction, query).await;
        //                         }
        //                     },
        //                     GitlabData::ProjectUsers(link) => {
        //                         //add relationship for every user given
        //                         for user in link.resource_vec as Vec<User>  {
        //                             let query = format!("MATCH (p:GitlabProject) WHERE p.project_id = '{}' with p MATCH (u:GitlabProject) WHERE u.user_id = '{}' with p, u MERGE (u)-[:onProject]->(p)", link.resource_id, user.id);
        //                             run_query(&transaction, query).await; 
        //                         }
        //                     },
        //                     GitlabData::ProjectRunners(link) => {
        //                         for runner in link.resource_vec as Vec<Runner> {
        //                             let query = format!("MATCH (n:GitlabProject) WHERE n.project_id = '{}' with n MATCH (r:GitlabRunner) WHERE r.runner_id = '{}' with n, r MERGE (r)-[:onProject]->(n)", link.resource_id, runner.id);
        //                             run_query(&transaction, query).await;
        //                         }
        //                     },
        //                     _ => todo!()  
        //                 }
        //                 if let Err(e) = transaction.commit().await {
        //                     let err_msg = format!("Error committing transaction to graph: {e}");
        //                     error!("{err_msg}");
        //                     todo!("What to do if we fail to commit queries?");
        //                 }
        //             },
        //             Err(e) => {
        //                 error!("Could not open transaction with graph! {e}");
        //                 todo!("Couldn't open transaction, What to do if we fail to commit queries?");
        //             }
        //         }                
        //     },
        //     _ => todo!("Gitlab consumer shouldn't get anything but gitlab data")
        // }
        Ok(())
    }
}