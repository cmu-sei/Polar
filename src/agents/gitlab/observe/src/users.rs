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


use core::error;
use std::time::Duration;

use crate::{GitlabObserverArgs, GitlabObserverMessage, GitlabObserverState, BROKER_CLIENT_NAME, PRIVATE_TOKEN_HEADER_STR};
use cassini::client::TcpClientMessage;
use cassini::ClientMessage;
use common::USERS_QUEUE_NAME;
use cynic::{GraphQlResponse, Id, Operation, QueryFragment, QueryVariables};
use ractor::concurrency::Interval;
use ractor::rpc::{call, CallResult};
use ractor::RpcReplyPort;
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use reqwest::{Client, Method, StatusCode};

use common::types::{User, GitlabData};
use tokio::time;
use tracing::{debug, info, warn, error};
use gitlab_queries::*;
use cynic::QueryBuilder;
use rkyv::rancor::Error;

pub struct GitlabUserObserver;

#[async_trait]
impl Actor for GitlabUserObserver {
    type Msg = GitlabObserverMessage;
    type State = GitlabObserverState;
    type Arguments = GitlabObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabObserverArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        
        match Client::builder().build() {
            Ok(client) => {
                let state = GitlabObserverState {
                    gitlab_endpoint: args.gitlab_endpoint,
                    token: args.token,
                    web_client:
                    client.clone(),
                    registration_id: args.registration_id
                };
                Ok(state)
            }
            Err(e) => {
                error!("{e}");
                Err(Box::new(e))
            }
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
        info!("{myself:?} Started");
        
        myself.send_interval(Duration::from_secs(10), || { 
            //TODO: get query arguments from config params
            //build query
            let op = MultiUserQuery::build(MultiUserQueryArguments{ after: None, admins: Some(true), active: None, ids: None, usernames: None, humans: Some(true) });

            // pass query in message
            GitlabObserverMessage::GetUsers(op) 
        });

        // match call(&&myself.get_cell(), |reply: RpcReplyPort<Result<(), String>>|  { 
        //     //TODO: get query arguments from config params
        //     //build query
        //     let op = MultiUserQuery::build(MultiUserQueryArguments{ after: None, admins: Some(true), active: None, ids: None, usernames: None, humans: Some(true) });

        //     // pass query in message
        //     GitlabObserverMessage::GetUsers(op) 
        //     } , None)
        // .await.expect("expected to call actor: {observer:?}") {
        //     CallResult::Success(result) => {
        //         if let Err(e) = result {
        //         let err_msg = format!("Failed to gather user data {e}");
        //         warn!("{err_msg}");
        //         myself.stop(Some(err_msg));
        //         }
        //     }
        //     CallResult::Timeout => error!("timed out sending message"),
        //     CallResult::SenderError => error!("Failed to send message")   
        // }
        
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            GitlabObserverMessage::GetUsers(op) =>  {

                debug!("Sending query: {op:?}");

                match state.web_client
                .post(state.gitlab_endpoint.clone())
                .bearer_auth(state.token.clone().unwrap_or_default())
                .json(&op)
                .send().await {
                    Ok(response) => {
                    
                        //TODO: take json and send to broker
                        //forwrard to client
                        match response.json::<GraphQlResponse<MultiUserQuery>>().await {
                            Ok(deserialized) => {
                                if let Some(query) = deserialized.data {

                                    if let Some(connection) = query.users {
                                        
                                        info!("Found {} user(s)", connection.count);

                                        let mut read_users: Vec<UserCore> = Vec::new();

                                        if let Some(users) = connection.nodes {
                                            // Append nodes to the result list.
                                            read_users.extend(users.into_iter().map(|option| option.unwrap()));
                                        }
                                        //serialize list to byte vec
                                        let byte_vec = rkyv::to_bytes::<Error>(&read_users).unwrap();
                                        //TODO:                                            
                                        // if connection.pageInfo.has_next_page {
                                        //     todo!("TODO: crawl pages and build list of UserCore types")
                                        // }

                                        match where_is(BROKER_CLIENT_NAME.to_string()) {
                                            Some(client) => {
                                                let data = GitlabData::Users(read_users);
                                                // Serializing is as easy as a single function call
                                                let bytes = rkyv::to_bytes::<Error>(&data).unwrap();
                                                

                                                let msg = ClientMessage::PublishRequest { topic: USERS_QUEUE_NAME.to_string(), payload: bytes.to_vec(), registration_id: Some(state.registration_id.clone()) };
                                                if let Err(e) = client.send_message(TcpClientMessage::Send(msg)) {
                                                    todo!()
                                                }
                                            }
                                            None => {
                                                let err_msg = "Failed to locate tcp client";
                                                error!("{err_msg}");
                                                
                                                }   
                                        } 
                                    }
                                }    
                            }
                            Err(e) => {
                                error!("{e}");
                            }
                        }
                        

                    } Err(e) => error!("{e}")
                }
                
                

            }
            _ => todo!()
        }
        Ok(())
    }

}