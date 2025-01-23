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
use crate::{subscribe_to_topic, ConsumerMessage, GitlabConsumerArgs, GitlabConsumerState};
// use helpers::helpers::{get_neo_config, run_query};
use crate::run_query;

use common::{connect_to_rabbitmq, GITLAB_EXCHANGE_STR, USERS_QUEUE_NAME, USERS_ROUTING_KEY};

use log::{debug, error, info};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};




pub struct GitlabUserConsumer;

#[async_trait]
impl Actor for GitlabUserConsumer {
    type Msg = ConsumerMessage;
    type State = GitlabConsumerState;
    type Arguments = GitlabConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabConsumerArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");
        //subscribe to topic
        match subscribe_to_topic(args.registration_id, USERS_QUEUE_NAME.to_string()).await {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg = format!("Error subscribing to topic \"{USERS_QUEUE_NAME}\" {e}");
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
            ConsumerMessage::GitlabData(data_type) => {
                match data_type {
                    GitlabData::Users(users) => {
                        // build query from scratch
                        match state.graph.start_txn().await {
                            Ok(transaction)  => {
                                for user in users {
                                    //create new nodes
                                    let query = format!("MERGE (n:GitlabUser {{username: \"{}\", user_id: \"{}\" , created_at: \"{}\" , state: \"{}\"}}) return n", user.username.unwrap_or_default(), user.id.inner(), user.created_at.unwrap_or_default(), user.state);            
                                    run_query(&transaction, query).await;
                                }

                                if let Err(e) = transaction.commit().await {
                                    let err_msg = format!("Error committing transaction to graph: {e}");
                                    error!("{err_msg}");
                                    todo!("What to do if we fail to commit queries?");
                                }
                                
                            }
                            Err(e) => {
                                error!("Could not open transaction with graph! {e}");
                                todo!("What to do when we can't access the graph")
                            }
                        } 
                    }
                    _ => todo!()
                }
            },
            _ => todo!("Gitlab consumer shouldn't get anything but gitlab data")
        }
        Ok(())
    }
}