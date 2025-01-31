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
use crate::{merge_group_query, merge_project_query, subscribe_to_topic, GitlabConsumerArgs, GitlabConsumerState};
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
        match message {
            GitlabData::Users(users) => {
                // build cypher query
                match state.graph.start_txn().await {
                    Ok(transaction)  => {
                        
                        let mut_cypher_query = String::new();

                        let users_data = users
                        .iter()
                        .map(|user| {
                            format!(
                                "{{ username: \"{}\", user_id: \"{}\", created_at: \"{}\", state: \"{}\" }}",
                                user.username.clone().unwrap_or_default(),
                                user.id,
                                user.created_at.clone().unwrap_or_default(),
                                user.state
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",\n");


                        let projects_data = users
                        .iter()
                        .filter_map(|user| user.contributed_projects.as_ref())
                        .flat_map(|connection| connection.nodes.iter().flatten())
                        .map(|node| {
                            match node {
                                Some(project) => {
                                    format!(
                                        "{{ project_id: \"{}\", name: \"{}\", full_path: \"{}\", created_at: \"{}\", last_activity_at: \"{}\" }}",
                                        project.id,
                                        project.name,
                                        project.full_path,
                                        project.created_at.clone().unwrap_or_default(),
                                        project.last_activity_at.clone().unwrap_or_default()
                                    )
                                }
                                None => String::default()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(",\n");

                        // let groups_data = users
                        // .iter()
                        // .filter_map(|user| user.groups.as_ref())
                        // .flat_map(|connection| connection.nodes.iter().flatten())
                        // .map(|node| {
                        //     match node {
                        //         Some(group) => {
                        //             format!(
                        //                 r#"
                        //                 {{
                        //                     group_id: "{group_id}",
                        //                     full_name: "{full_name}",
                        //                     full_path: "{group_full_path}",
                        //                     created_at: "{group_created_at}",
                        //                     member_count: "{group_members_count}"
                        //                 }}
                        //                 "#,
                        //                 group_id = group.id, 
                        //                 full_name = group.full_name, 
                        //                 group_full_path = group.full_path,
                        //                 group_created_at = group.created_at.clone().unwrap_or_default(), 
                        //                 group_members_count = group.group_members_count,
                        //             )
                        //         }
                        //         None => String::default()
                        //     }
                        // })
                        // .collect::<Vec<_>>()
                        // .join(",\n");
                        //TODO: We can't query groups here because of the complexity score when groups are returned. Make these connections from the groups observer?
                        // UNWIND [{groups_data}] AS group_data
                        //     MERGE (group: GitlabGroupGitlabGroup {{ group_id: group_data.group_id }})
                        //     SET full_name: group_data.full_name,
                        //         full_path: group_data.full_path,
                        //         created_at: group_data.created_at,
                        //         member_count: group_data.group_member_count
                            
                        //     MERGE (user)-[:IN_GROUP]->(group);


                        // Final query string with `UNWIND`
                        let cypher_query = format!(
                            "
                            UNWIND [{users_data}] AS user_data
                            MERGE (user:GitlabUser {{ user_id: user_data.user_id }})
                            SET user.username = user_data.username,
                                user.created_at = user_data.created_at,
                                user.state = user_data.state
                            WITH user
                            UNWIND [{projects_data}] AS project_data
                            MERGE (project:GitlabProject {{ project_id: project_data.project_id }})
                            SET project.name = project_data.name,
                                project.full_path = project_data.full_path,
                                project.created_at = project_data.created_at,
                                project.last_activity_at = project_data.last_activity_at
                            
                            MERGE (user)-[:CONTRIBUTED_TO]->(project);
                            
                            "
                        );

                        

                        //TODO: Do the same for groups
                                               // let cypher_query: String = users.iter()
                        //     .map(|user|{ 
                            
                        //     // get projects
                        //     // let merge_projects: String = {
                        //     //     if let Some(project_connection) = &user.contributed_projects {       
                        //     //         if let Some(nodes) = &project_connection.nodes {
                        //     //             nodes.iter().map(|node| {
                        //     //                 match node {
                        //     //                     Some(project) => format!("{0} WITH user, project MERGE (user)-[:contributedTo]->(project)", merge_project_query(project.clone())),
                        //     //                     None => String::default()
                        //     //                 }
                        //     //             }).collect::<Vec<_>>().join("\n")

                        //     //         } else { String::default() }
                        //     //     } else { String::default() }
                        //     // };

                        //     // let merge_groups: String = {
                        //     //     // get user group connections
                        //     //     if let Some(group_connection) = &user.groups {
                        //     //         if let Some(nodes) = &group_connection.nodes {
                        //     //             nodes.iter().map(|node| {
                        //     //                 match node {
                        //     //                     Some(group) => format!(" {0} WITH user, group MERGE (user)-[:InGroup]->)group)", merge_group_query(group.clone())),
                        //     //                     None => String::d0efault()
                        //     //                 }
                        //     //             }).collect::<Vec<_>>().join("\n")
                        //     //         } else { String::default() }
                        //     //     } else { String::default() }
                        //     //{merge_groups}
                        //     // };

                        // })
                        // .collect::<Vec<_>>()
                        // .join("\n");


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
            _ => todo!("Gitlab consumer shouldn't get anything but gitlab data")
        }
        Ok(())
    }
}