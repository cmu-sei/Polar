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

use crate::{subscribe_to_topic, JiraConsumerArgs, JiraConsumerState};
use polar::{QUERY_COMMIT_FAILED, QUERY_RUN_FAILED, TRANSACTION_FAILED_ERROR};

use jira_common::types::JiraData;
use jira_common::JIRA_USERS_CONSUMER_TOPIC;
use neo4rs::Query;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, info};

pub struct JiraUserConsumer;

#[async_trait]
impl Actor for JiraUserConsumer {
    type Msg = JiraData;
    type State = JiraConsumerState;
    type Arguments = JiraConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: JiraConsumerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");
        match subscribe_to_topic(
            args.registration_id,
            JIRA_USERS_CONSUMER_TOPIC.to_string(),
            args.graph_config,
        )
        .await
        {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg = format!("Error starting actor: \"{JIRA_USERS_CONSUMER_TOPIC}\" {e}");
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
        match state.graph.start_txn().await {
            Ok(mut transaction) => {
                match message {
                    JiraData::Users(users) => {
                        // Create list of users
                        let user_array = users.iter()
                            .map(|user| {
                                format!(
                                    r#"{{ html: {:?}, name: {:?}, key:{:?}, displayName:{:?}}}"#,
                                    <Option<std::string::String> as Clone>::clone(&user.html).unwrap_or("".to_string()).clone(),
                                    user.name.to_string().clone(),
                                    user.key.to_string().clone(),
                                    user.displayName.to_string().clone(),
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(",\n");

                        // Here, we write a query that creates additional nodes for the user and namespace of the user
                        let cypher_query = format!(
                            "
                            UNWIND [{user_array}] AS user_data
                            MERGE (user:JiraUser {{ key: user_data.key }})
                            SET user.html = user_data.html, user.name = user_data.name, user.displayName = user_data.displayName
                            "
                        );

                        debug!(cypher_query);
                        if let Err(_e) = transaction.run(Query::new(cypher_query)).await {
                            myself.stop(Some(QUERY_RUN_FAILED.to_string()));
                        }

                        if let Err(_e) = transaction.commit().await {
                            myself.stop(Some(QUERY_COMMIT_FAILED.to_string()));
                        }
                        info!("Transaction committed.");
                    }
                    _ => (),
                }
            }
            Err(e) => myself.stop(Some(format!("{TRANSACTION_FAILED_ERROR}. {e}"))),
        }
        Ok(())
    }
}
