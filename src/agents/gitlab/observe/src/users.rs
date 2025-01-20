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

use crate::{ get_all_elements, send, GitlabObserverArgs, GitlabObserverMessage, GitlabObserverState, BROKER_CLIENT_NAME};
use cynic::{Operation, QueryFragment, QueryVariables};
use ractor::RpcReplyPort;
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use reqwest::Client;
use common::USERS_QUEUE_NAME;
use common::types::{User, GitlabData};
use tracing::{debug, info, warn, error};
use gitlab_queries::*;

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


        // TODO: initiate loops based off of provided schedule, resources should be retrieved on some given interval            
        // myself.send_interval(Duration::from_secs(3), || { GitlabObserverMessage::GetUsers });
        
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
                let users: Vec<User> = get_all_elements(&state.web_client, state.token.clone().unwrap_or_default(), format!("{}{}", state.gitlab_endpoint, "/users"))
                .await
                .expect("Expected to get user data");
            
                
                debug!("Retrieved {} users", users.len());
                
                //forwrard to client
                if let Some(client) = where_is(BROKER_CLIENT_NAME.to_string()) {
                    let data = GitlabData::Users(users);
                    match send(data, client.into(), state.registration_id.clone(), USERS_QUEUE_NAME.to_string()) {
                        Ok(_) => {
                            debug!("Successfully sent user data");
                        } Err(e) => todo!()
                    }
                }
            }
            _ => todo!()
        }
        Ok(())
    }

}