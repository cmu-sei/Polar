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

use std::{error::Error, fs::remove_file};
use crate::{ get_all_elements, send, BROKER_CLIENT_NAME};
use cassini::{client::TcpClientMessage, ClientMessage};
use lapin::{options::{QueueDeclareOptions, QueueBindOptions}, types::FieldTable};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use reqwest::Client;
use serde_json::to_string;
use common::USERS_QUEUE_NAME;
use common::types::{User, GitlabData};
use log::{debug, info, warn};
const LOCK_FILE_PATH: &str = "/tmp/users_observer.lock";


pub struct GitlabUserObserver;

pub struct GitlabUserObserverState {
    gitlab_endpoint: String, //endpoint of gitlab instance
    token: Option<String>, //token for auth,
    web_client: Client,
    registration_id: String
}


pub struct GitlabUserObserverArgs {
    pub gitlab_endpoint: String, //endpoint of gitlab instance
    pub token: Option<String>, //token for auth
    pub registration_id: String, //id of agent's current session with the broker
}
#[async_trait]
impl Actor for GitlabUserObserver {
    type Msg = ();
    type State = GitlabUserObserverState;
    type Arguments = GitlabUserObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabUserObserverArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to instance");
        
        match Client::builder().build() {
            Ok(client) => {
                let state = GitlabUserObserverState {
                    gitlab_endpoint: args.gitlab_endpoint,
                    token: args.token,
                    web_client:
                    client.clone(),
                    registration_id: args.registration_id
                };
                Ok(state)
            }
            Err(e) => Err(Box::new(e))
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State ) ->  Result<(), ActorProcessingErr> {

        //TODO: use client in state to pull gitlab user data
        let users: Vec<User> = get_all_elements(&state.web_client, state.token.clone().unwrap_or_default(), format!("{}{}", state.gitlab_endpoint, "/users")).await.unwrap();
        
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
        
        myself.stop(Some("FINISHED".to_string()));
        
        Ok(())
    }
    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        _: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        

        Ok(())
    }

}