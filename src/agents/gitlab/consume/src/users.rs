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

use std::fmt::Display;

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
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
            info!("{:?} waiting to consume", myself.get_name());
        
        Ok(())
    }
    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            GitlabData::Users(users) => {
                //TODO: Expect transaction to start, panic if it doesn't
                match state.graph.start_txn().await {
                    Ok(mut transaction)  => {
                        
                        let mut_cypher_query = String::new();

                        let users_data = users
                        .iter()
                        .map(|user| {
                            format!(
                                "{{ username: \"{username}\", user_id: \"{user_id}\", created_at: \"{created_at}\", state: \"{state}\" }}",
                                username = user.username.clone().unwrap_or_default(),
                                user_id = user.id,
                                created_at = user.created_at.clone().unwrap_or_default(),
                                state = user.state
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",\n");

                        let cypher_query = format!(
                            "
                            UNWIND [{users_data}] AS user_data
                            MERGE (user:GitlabUser {{ user_id: user_data.user_id }})
                            SET user.username = user_data.username,
                                user.created_at = user_data.created_at,
                                user.state = user_data.state
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
            GitlabData::ProjectMembers(link) => {
                match state.graph.start_txn().await {
                    Ok(mut transaction) => {
                        let nodes = link.connection.nodes.unwrap();
                        

                        let project_memberships = nodes
                        .iter()
                        .filter_map(|option| {
                            let membership = option.as_ref().unwrap();
                            
                            //create a list of attribute sets that will represent the relationship between a user and each project
                            membership.project.as_ref().map(|project| {
                                format!(
                                    r#"{{
                                        project_id: "{}",
                                        access_level: "{}",
                                        created_at: "{}",
                                        expires_at: "{}"
                                    }}"#,
                                    project.id,
                                    //TODO: Represent this as a string, Too annoying to get the string value of this right now
                                    membership.access_level.as_ref().map_or_else(|| String::default(), |al| {
                                        al.integer_value.unwrap_or_default().to_string()
                                    }),
                                    membership.created_at.as_ref().map_or_else(|| String::default(), |date| date.to_string()),
                                    membership.expires_at.as_ref().map_or_else(|| String::default(), |date| date.to_string()),
                                )
                            })
                        })
                        .collect::<Vec<_>>()
                        .join(",\n");
                        

                        //write a query that finds the given user, and create a relationship between it and every project we were given
                        let cypher_query = format!(
                            "
                            MATCH (user:GitlabUser {{ user_id: \"{}\" }})
                            UNWIND [{}] AS project_data
                            MERGE (project:GitlabProject {{ project_id: project_data.project_id }})
                            MERGE (user)-[r:MEMBER_OF]->(project)
                            SET r.access_level = project_data.access_level,
                                r.created_at = project_data.created_at,
                                r.expires_at = project_data.expires_at
                            ",
                            link.resource_id, project_memberships
                        );
            
                        debug!(cypher_query);
                        transaction.run(Query::new(cypher_query)).await.expect("Expected to run query.");
                        if let Err(e) = transaction.commit().await {
                            error!("Error committing transaction to graph: {e}");
                        }
                        info!("Committed transaction to database");
                    }
                    Err(e) => {
                        error!("Could not open transaction with graph! {e}");
                    }
                }
            }
            
            _ => todo!("Gitlab consumer shouldn't get anything but gitlab data")
        }
        Ok(())
    }
}
