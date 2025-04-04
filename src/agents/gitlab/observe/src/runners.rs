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

use std::time::Duration;

use cassini::{client::TcpClientMessage, ClientMessage};
use common::{types::GitlabData, RUNNERS_CONSUMER_TOPIC};
use cynic::{GraphQlResponse, QueryBuilder};
use gitlab_queries::runners::*;

use crate::{get_all_elements, GitlabObserverArgs, GitlabObserverMessage, GitlabObserverState};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use reqwest::Client;
use serde_json::to_string;
use tracing::{debug, error, info, warn};

use crate::BROKER_CLIENT_NAME;

pub struct GitlabRunnerObserver;

#[async_trait]
impl Actor for GitlabRunnerObserver {
    type Msg = GitlabObserverMessage;
    type State = GitlabObserverState;
    type Arguments = GitlabObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabObserverArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to instance");

        let state = GitlabObserverState {
            gitlab_endpoint: args.gitlab_endpoint,
            token: args.token,
            web_client: args.web_client,
            registration_id: args.registration_id,
        };
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        myself.send_interval(Duration::from_secs(10), || {
            //TODO: get query arguments from config params
            //build query
            let op = MultiRunnerQuery::build(MultiRunnerQueryArguments {
                paused: Some(false),
                status: None,
                tag_list: None,
                search: None,
                creator_id: None,
            });

            // pass query in message
            GitlabObserverMessage::GetRunners(op)
        });
        Ok(())
    }
    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            GitlabObserverMessage::GetRunners(op) => {
                debug!("Sending query: {:?}", op.query);

                match state
                    .web_client
                    .post(state.gitlab_endpoint.clone())
                    .bearer_auth(state.token.clone().unwrap_or_default())
                    .json(&op)
                    .send()
                    .await
                {
                    Ok(response) => {
                        if let Ok(deserialized) =
                            response.json::<GraphQlResponse<MultiRunnerQuery>>().await
                        {
                            if let Some(errors) = deserialized.errors {
                                for error in errors {
                                    warn!("Received errors, {error:?}");
                                }
                            }
                            if let Some(query) = deserialized.data {
                                if let Some(connection) = query.runners {
                                    let mut read_runners = Vec::new();

                                    if let Some(runners) = connection.nodes {
                                        for option in runners {
                                            if let Some(runner) = option {
                                                read_runners.push(runner);
                                            }
                                        }
                                    }

                                    info!("Observed {0} runner(s)", read_runners.len());

                                    let client = where_is(BROKER_CLIENT_NAME.to_string())
                                        .expect("Expected to find tcp client!");

                                    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(
                                        &GitlabData::Runners(read_runners),
                                    )
                                    .unwrap();

                                    let msg = ClientMessage::PublishRequest {
                                        topic: RUNNERS_CONSUMER_TOPIC.to_string(),
                                        payload: bytes.to_vec(),
                                        registration_id: Some(state.registration_id.clone()),
                                    };
                                    client
                                        .send_message(TcpClientMessage::Send(msg))
                                        .expect("Expected to send message to tcp client!");
                                }
                            }
                        }
                    }
                    Err(e) => todo!(),
                }
            }
            _ => (),
        }

        Ok(())
    }
}

// #[tokio::main]
// async fn main() -> Result<(), Box<dyn Error> > {
//     let result = create_lock(LOCK_FILE_PATH);
//     match result {
//         Err(e) => panic!("{}", e),
//         Ok(false) => Ok(()),
//         Ok(true) => {
//             info!("running runners task");

//             //publish user id and list of user's projects to queue?
//             let mq_conn = connect_to_rabbitmq().await?;

//             //create publish channel

//             let mq_publish_channel = mq_conn.create_channel().await?;

//             //create fresh queue, empty string prompts the server backend to create a random name
//             let _ = mq_publish_channel.queue_declare(RUNNERS_QUEUE_NAME,QueueDeclareOptions::default() , FieldTable::default()).await?;

//             //bind queue to exchange so it sends messages where we need them
//             mq_publish_channel.queue_bind(RUNNERS_QUEUE_NAME, GITLAB_EXCHANGE_STR, RUNNERS_ROUTING_KEY, QueueBindOptions::default(), FieldTable::default()).await?;

//             //poll gitlab for available users
//             let gitlab_token = get_gitlab_token();
//             let service_endpoint = get_gitlab_endpoint();

//             let web_client = helpers::helpers::web_client();

//             let _ = mq_conn.close(0, "closed").await?;

//             std::fs::remove_file(LOCK_FILE_PATH).expect("Error deleting lock file");

//             Ok(())
//         }
//     }
// }
