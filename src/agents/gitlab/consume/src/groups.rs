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
use common::types::{GitlabData};
use neo4rs::Query;
use crate::{subscribe_to_topic, GitlabConsumerArgs, GitlabConsumerState};
use common::{GROUPS_CONSUMER_TOPIC};
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
        //subscribe to topic
        match subscribe_to_topic(args.registration_id, GROUPS_CONSUMER_TOPIC.to_string()).await {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg = format!("Error subscribing to topic {GROUPS_CONSUMER_TOPIC} {e}");
                Err(ActorProcessingErr::from(err_msg))
            }
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
        debug!("{myself:?} started. Waitng to consume.");
        Ok(())
    }
    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {

        match message {
            GitlabData::Groups(vec) => {
                let transaction = state.graph.start_txn().await.expect("Expected to start a transaction with the graph.");

                for g in vec {
                    let query = format!(r#"
                    MERGE (group: GitlabGroup {{ group_id: "{id}" }})
                    SET group.full_name = "{full_name}",
                        group.full_path = "{full_path}",
                        group.created_at = "{created_at}"
                        "#, 
                        id = g.id, 
                        full_name = g.full_name, 
                        full_path = g.full_path,
                        created_at = g.created_at.unwrap_or_default(), 
                    );
                    debug!(query);
                    transaction.run(Query::new(query)).await.expect("Expected to run query on transaction.");
                }

                transaction.commit().await.expect("Expected to commit transaction");
            },
            GitlabData::GroupMembers(link) => {
                let transaction = state.graph.start_txn().await.expect("expected transaction");
                
                if let Some(vec)  =  link.connection.nodes {
                    let group_memberships = vec
                    .iter()
                    .map(|option| {
                        let membership = option.as_ref().unwrap();

                        //create a list of attribute sets
                        format!(
                            r#"{{
                                user_id: "{user_id}",
                                access_level: "{access_level}",
                                created_at: "{created_at}",
                                updated_at: "{updated_at}",
                                expires_at: "{expires_at}"
                            }}"#,
                            user_id = membership.user.as_ref().map_or_else(|| String::default(), |user| user.id.to_string()),
                            //TODO: Represent this as a string, Too annoying to get the string value of this right now
                            access_level = membership.access_level.as_ref().map_or_else(|| String::default(), |al| {
                                al.integer_value.unwrap_or_default().to_string()
                            }),
                            created_at = membership.created_at.as_ref().map_or_else(|| String::default(), |date| date.to_string()),
                            updated_at = membership.updated_at.as_ref().map_or_else(|| String::default(), |date| date.to_string()),
                            expires_at = membership.expires_at.as_ref().map_or_else(|| String::default(), |date| date.to_string()),
                            
                        )
                        })
                    .collect::<Vec<_>>()
                    .join(",\n");
        
                    let cypher_query = format!(
                        "
                        MATCH (group:GitlabGroup {{ group_id: \"{group_id}\" }})
                        UNWIND [{group_memberships}] AS membership
                        MERGE (user:GitlabUser {{ user_id: membership.user_id }})
                        MERGE (user)-[r:IN_GROUP]->(group)
                        SET r.access_level = membership.access_level,
                            r.created_at = membership.created_at,
                            r.expires_at = membership.expires_at,
                            r.updated_at = membership.updated_at
                        ",
                        group_id = link.resource_id
                    );
        
                    debug!(cypher_query);
                    transaction.run(Query::new(cypher_query)).await.expect("Expected to run query.");
                    if let Err(e) = transaction.commit().await {
                        error!("Error committing transaction to graph: {e}");
                    }
                    info!("Committed transaction to database");
                }
            }
            GitlabData::GroupProjects(link) => {
                let transaction = state.graph.start_txn().await.expect("expected transaction");
                
                if let Some(vec)  =  link.connection.nodes {
                    let projects = vec
                    .iter()
                    .map(|option| {
                        let project = option.as_ref().unwrap();

                        //create a list of attribute sets
                        format!(r#"{{ project_id: "{project_id}" }}"#, project_id = project.id )
                    })
                    .collect::<Vec<_>>()
                    .join(",\n");
        
                    let cypher_query = format!(
                        "
                        MATCH (group:GitlabGroup {{ group_id: \"{group_id}\" }})
                        UNWIND [{projects}] AS project_data
                        MERGE (project:GitlabProject {{ project_id: project_data.project_id }})
                        MERGE (project)-[r:IN_GROUP]->(group)
                        ",
                        group_id = link.resource_id
                    );
        
                    debug!(cypher_query);
                    transaction.run(Query::new(cypher_query)).await.expect("Expected to run query.");
                    if let Err(e) = transaction.commit().await {
                        error!("Error committing transaction to graph: {e}");
                    }
                    info!("Committed transaction to database");
                }
            }
            GitlabData::GroupRunners(link) => {
                let transaction = state.graph.start_txn().await.expect("expected transaction");
                
                if let Some(vec)  =  link.connection.nodes {
                    let runners = vec
                    .iter()
                    .map(|option| {
                        let runner = option.as_ref().unwrap();

                        //create a list of attribute sets
                        format!(r#"{{ 
                            runner_id: "{runner_id}",
                            paused: "{paused}",
                        }}"#,
                        runner_id = runner.id.0,
                        paused = runner.paused
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",\n");
        
                    let cypher_query = format!(
                        "
                        MATCH (group:GitlabGroup {{ group_id: \"{group_id}\" }})
                        UNWIND [{runners}] AS runner_data
                        MERGE (runner:GitlabRunner {{ project_id: runner_data.runner_id }})
                        MERGE (runner)-[r:IN_GROUP]->(group)
                        ",
                        group_id = link.resource_id
                    );
        
                    debug!(cypher_query);
                    transaction.run(Query::new(cypher_query)).await.expect("Expected to run query.");
                    if let Err(e) = transaction.commit().await {
                        error!("Error committing transaction to graph: {e}");
                    }
                    info!("Committed transaction to database");
                }
            }
            
            _ => ()
        }
        Ok(())
    }
}