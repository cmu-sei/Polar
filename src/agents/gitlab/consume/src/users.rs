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
use common::{USER_CONSUMER_TOPIC};
use tracing::{debug, error, field::debug, info, warn};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};


pub struct GitlabUserConsumer;

#[async_trait]
impl Actor for GitlabUserConsumer {
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
        match subscribe_to_topic(args.registration_id, USER_CONSUMER_TOPIC.to_string()).await {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg = format!("Error subscribing to topic \"{USER_CONSUMER_TOPIC}\" {e}");
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
        //TODO: Implement message type for consumers to handle new messages
        match message {
            GitlabData::Users(users) => {
                // build query from scratch
                match state.graph.start_txn().await {
                    Ok(transaction)  => {
                        for user in users {
                            //create new nodes
                            let query = format!("MERGE (n:GitlabUser {{username: \"{}\", user_id: \"{}\" , created_at: \"{}\" , state: \"{}\"}}) return n", user.username.unwrap_or_default(), user.id, user.created_at.unwrap_or_default(), user.state);            
                            if let Err(e) = transaction.run(Query::new(query)).await {
                                warn!("Failed to run query! {e}");
                            }

                            if let Some(project_connection) = user.contributed_projects {
                                
                                if let Some(nodes) = project_connection.nodes {
                                    for node in nodes {
                                        let project = node.unwrap();
                                        let namespace = project.namespace.unwrap();

                                        let cypher_query = format!(
                                            r#"
                                            MERGE (p: GitlabProject {{ 
                                                project_id: "{project_id}", 
                                                name: "{project_name}",
                                                created_at: "{created_at}"
                                            }})
                                            MERGE (n: GitlabNamespace {{
                                                namespace_id: "{namespace_id}",
                                                full_name: "{full_name}",
                                                full_path: "{full_path}"
                                            }})
                                            WITH p, n MERGE (p)-[:inNamespace]->(n)
                                            WITH p, n MATCH (u:GitlabUser) WHERE u.user_id = '{user_id}'
                                            WITH p, u MERGE (u)-[:contributedTo]->(p)
                                            "#,
                                            project_id = project.id,
                                            project_name = project.name,
                                            created_at = project.created_at.unwrap_or_default(),
                                            user_id = user.id,
                                            namespace_id = namespace.id,
                                            full_name = namespace.full_name,
                                            full_path = namespace.full_path,
                                        );
                                        debug!(cypher_query);

                                        if let Err(e) = transaction.run(Query::new(cypher_query)).await {
                                            warn!("Failed to run query!");
                                        }
                                    }
                                }
                            }
                        }

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
            _ => todo!("Gitlab consumer shouldn't get anything but gitlab data")
        }
        Ok(())
    }
}