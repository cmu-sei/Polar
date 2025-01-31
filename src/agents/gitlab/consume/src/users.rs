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
                        
                        let cypher_query: String = users.iter()
                            .map(|user|{ 
                            //create new nodes
                            
                            let merge_user = format!("
                                    MERGE (n:GitlabUser {{ 
                                        username: \"{}\",
                                        user_id: \"{}\" ,
                                        created_at: \"{}\",
                                        state: \"{}\"
                                    }})\n",
                                    user.username.clone().unwrap_or_default(),
                                    user.id,
                                    user.created_at.clone().unwrap_or_default(),
                                    user.state
                            );
                            // get projects
                            let merge_projects: String = {
                                if let Some(project_connection) = &user.contributed_projects {       
                                    if let Some(nodes) = &project_connection.nodes {
                                        nodes.iter().map(|node| {
                                            match node {
                                                Some(project) => format!("{0} WITH user, project MERGE (user)-[:contributedTo]->)project)", merge_project_query(project.clone())),
                                                None => String::default()
                                            }
                                        }).collect::<Vec<_>>().join("\n")

                                    } else { String::default() }
                                } else { String::default() }
                            };

                            let merge_groups: String = {
                                // get user group connections
                                if let Some(group_connection) = &user.groups {
                                    if let Some(nodes) = &group_connection.nodes {
                                        nodes.iter().map(|node| {
                                            match node {
                                                Some(group) => format!(" {0} WITH user, group MERGE (user)-[:InGroup]->)group)", merge_group_query(group.clone())),
                                                None => String::default()
                                            }
                                        }).collect::<Vec<_>>().join("\n")
                                    } else { String::default() }
                                } else { String::default() }
                            };
                            format!("{merge_user} \n {merge_projects} \n {merge_groups}")
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

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