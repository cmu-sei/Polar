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

use crate::{ get_all_elements, send, GitlabObserverArgs, GitlabObserverMessage, GitlabObserverState, BROKER_CLIENT_NAME, PRIVATE_TOKEN_HEADER_STR};
use cynic::{GraphQlResponse, Id, Operation, QueryFragment, QueryVariables};
use ractor::rpc::{call, CallResult};
use ractor::RpcReplyPort;
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use reqwest::{Client, Method, StatusCode};
use common::USERS_QUEUE_NAME;
use common::types::{User, GitlabData};
use tokio::time;
use tracing::{debug, info, warn, error};
use gitlab_queries::*;
use cynic::QueryBuilder;

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

    
        let mut interval = time::interval(Duration::from_secs(10));
        let handle = tokio::task::spawn(async move {
            loop {
                match call(&&myself.get_cell(), |reply: RpcReplyPort<Result<(), String>>|  { 
                    //TODO: get query arguments from config params
                    //build query
                    let op = MultiUserQuery::build(MultiUserQueryArguments{ after: None, admins: Some(true), active: None, ids: None, usernames: None, humans: Some(true) });
        
                    // pass query in message
                    GitlabObserverMessage::GetUsers(reply, op) 
                    } , None)
                .await.expect("expected to call actor: {observer:?}") {
                    CallResult::Success(result) => {
                        if let Err(e) = result {
                        let err_msg = format!("Failed to gather user data {e}");
                        warn!("{err_msg}");
                        myself.stop(Some(err_msg));
                        }
                    }
                    CallResult::Timeout => error!("timed out sending message"),
                    CallResult::SenderError => error!("Failed to send message")   
                }
                interval.tick().await;
            }
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
            GitlabObserverMessage::GetUsers(reply, op) =>  {

                debug!("Sending query: {op:?}");

                match state.web_client
                .post(state.gitlab_endpoint.clone())
                .bearer_auth(state.token.clone().unwrap_or_default())
                .json(&op)
                .send().await {
                    Ok(response) => {
                        if response.status() == StatusCode::OK {
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
                                            
                                            //TODO:                                            
                                            // if connection.pageInfo.has_next_page {
                                            //     todo!("TODO: crawl pages and build list of UserCore types")
                                            // }

                                            match where_is(BROKER_CLIENT_NAME.to_string()) {
                                                Some(client) => {
                                                    let data = GitlabData::Users(read_users);
                                                    if let Err(e) = send(data, client.into(), state.registration_id.clone(), USERS_QUEUE_NAME.to_string()) {
                                                        let err_msg = format!("Failed to send message. {e}");
                                                        error!("{err_msg}");
                                                        reply.send(Err(format!("{err_msg}"))).unwrap();
                                                    }
                                                    else { reply.send(Ok(())).unwrap() }
                                                }
                                                None => {
                                                    let err_msg = "Failed to locate tcp client";
                                                    error!("{err_msg}");
                                                    reply.send(Err(format!("{err_msg}"))).unwrap();
                                                 }   
                                            } 
                                        } else { reply.send(Ok(())).unwrap() }
                                    }    
                                }
                                Err(e) => {
                                    error!("{e}");
                                    reply.send(Err(e.to_string())).unwrap();
                                }
                            }
                        } else { reply.send(Err(format!("{:?}", response))).unwrap() }

                    } Err(e) => error!("{e}")
                }
                
                

            }
            _ => todo!()
        }
        Ok(())
    }

}