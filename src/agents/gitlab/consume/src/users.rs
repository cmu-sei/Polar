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

use crate::{subscribe_to_topic, GitlabConsumerArgs, GitlabConsumerState};
use polar::{QUERY_COMMIT_FAILED, QUERY_RUN_FAILED, TRANSACTION_FAILED_ERROR};

use common::types::{GitlabData, GitlabEnvelope};
use common::USER_CONSUMER_TOPIC;
use gitlab_schema::DateString;
use neo4rs::Query;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, error, info};

pub struct GitlabUserConsumer;

#[async_trait]
impl Actor for GitlabUserConsumer {
    type Msg = GitlabEnvelope;
    type State = GitlabConsumerState;
    type Arguments = GitlabConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabConsumerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");
        //subscribe to topic
        match subscribe_to_topic(
            args.registration_id,
            USER_CONSUMER_TOPIC.to_string(),
            args.graph_config,
        )
        .await
        {
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
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("{:?} waiting to consume", myself.get_name());

        Ok(())
    }
    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        //Expect transaction to start, stop if it doesn't
        match state.graph.start_txn().await {
            Ok(mut transaction) => {
                match message.data {
                    GitlabData::Users(users) => {
                        let users_data = users
                            .iter()
                            .map(|user| {
                                format!(
                                    r#"{{
                                    user_id: "{user_id}",
                                    username: "{username}",
                                    bot: "{bot}",
                                    web_url: "{web_url}",
                                    web_path: "{web_path}",
                                    created_at: "{created_at}",
                                    state: "{state}",
                                    last_activity_on: "{last_activity_on}",
                                    location: "{location}",
                                    organization: "{organization}"
                                 }}"#,
                                    username = user.username.clone().unwrap_or_default(),
                                    user_id = user.id,
                                    created_at = user.created_at.clone().unwrap_or_default(),
                                    state = user.state,
                                    bot = user.bot,
                                    last_activity_on = user
                                        .last_activity_on
                                        .clone()
                                        .unwrap_or(DateString(String::default())),
                                    web_path = user.web_path,
                                    web_url = user.web_url,
                                    location = user.location.clone().unwrap_or_default(),
                                    organization = user.organization.clone().unwrap_or_default()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(",\n");

                        let cypher_query = format!(
                            r#"
                            UNWIND [{users_data}] AS user_data
                            MERGE (user:GitlabUser {{ user_id: user_data.user_id }})
                            SET user.username = user_data.username,
                                user.created_at = user_data.created_at,
                                user.state = user_data.state,
                                user.bot = user_data.bot,
                                user.location = user_data.location,
                                user.web_url = user_data.web_url,
                                user.web_path = user_data.web_path,
                                user.organization = user_data.organization
                            WITH user
                            MERGE (instance: GitlabInstance {{instance_id: "{}" }})
                            WITH user, instance
                            MERGE (instance)-[:OBSERVED_USER]->(user)
                            "#,
                            message.instance_id
                        );

                        debug!(cypher_query);
                        if let Err(e) = transaction.run(Query::new(cypher_query)).await {
                            error!("{e}");
                            myself.stop(Some(e.to_string()));
                        }

                        if let Err(e) = transaction.commit().await {
                            error!("{e}");
                            myself.stop(Some(QUERY_COMMIT_FAILED.to_string()))
                        }

                        info!("Committed transaction to database");
                    }
                    GitlabData::ProjectMembers(link) => {
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
                                        membership.access_level.as_ref().map_or_else(
                                            || String::default(),
                                            |al| {
                                                al.integer_value.unwrap_or_default().to_string()
                                            }
                                        ),
                                        membership.created_at.as_ref().map_or_else(
                                            || String::default(),
                                            |date| date.to_string()
                                        ),
                                        membership.expires_at.as_ref().map_or_else(
                                            || String::default(),
                                            |date| date.to_string()
                                        ),
                                    )
                                })
                            })
                            .collect::<Vec<_>>()
                            .join(",\n");

                        // TODO: We should check this on the observer side
                        if !project_memberships.is_empty() {
                            //write a query that finds the given user, and create a relationship between it and every project we were given
                            let cypher_query = format!(
                                r#"
                                MERGE (user:GitlabUser {{ user_id: "{}" }})
                                WITH user
                                UNWIND [{project_memberships}] AS project_data
                                MERGE (project:GitlabProject {{ project_id: project_data.project_id }})
                                MERGE (user)-[r:MEMBER_OF]->(project)
                                SET r.access_level = project_data.access_level,
                                    r.created_at = project_data.created_at,
                                    r.expires_at = project_data.expires_at
                                "#,
                                link.resource_id,
                            );

                            debug!(cypher_query);

                            if let Err(e) = transaction.run(Query::new(cypher_query)).await {
                                error!("{e}");
                                myself.stop(Some(QUERY_RUN_FAILED.to_string()));
                            }

                            if let Err(e) = transaction.commit().await {
                                error!("{e}");
                                myself.stop(Some(QUERY_COMMIT_FAILED.to_string()));
                            }

                            info!("Committed transaction to database");
                        }
                    }
                    _ => tracing::warn!("Unexpected message {:?}", message.instance_id),
                }
            }
            Err(e) => myself.stop(Some(format!("{TRANSACTION_FAILED_ERROR}. {e}"))),
        }

        Ok(())
    }
}
