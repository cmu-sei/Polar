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

use cassini::{client::TcpClientMessage, ClientMessage};
use common::PROJECTS_QUEUE_NAME;

use crate::{get_all_elements, send, GitlabObserverArgs, GitlabObserverState, BROKER_CLIENT_NAME};

use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use reqwest::Client;
use serde_json::to_string;
use common::{types::Project, RUNNERS_QUEUE_NAME, USERS_QUEUE_NAME};
use common::types::{GitlabData, Pipeline, ResourceLink, Runner, User};
use std::{error::Error, fs::remove_file};
use log::{debug, error, info, warn};


const LOCK_FILE_PATH: &str = "/tmp/projects_observer.lock";

pub struct GitlabProjectObserver;

#[async_trait]
impl Actor for GitlabProjectObserver {
    type Msg = ();
    type State = GitlabObserverState;
    type Arguments = GitlabObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabObserverArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to instance");
        
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
            Err(e) => Err(Box::new(e))
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State ) ->  Result<(), ActorProcessingErr> {

        if let Some(client) = where_is(BROKER_CLIENT_NAME.to_string()) {
            let client_ref: ActorRef<TcpClientMessage> = ActorRef::from(client);

            if let Some(projects) = get_all_elements::<Project>(&state.web_client, state.token.clone().unwrap_or_default(), format!("{}{}", state.gitlab_endpoint.clone(), "/projects")).await {
                // send initial list of all projects
                let data = GitlabData::Projects(projects.clone());
                match send(data, client_ref.clone(), state.registration_id.clone(), PROJECTS_QUEUE_NAME.to_string()) {
                    Ok(_) => {
                        debug!("Successfully sent project data");
                    } Err(e) => todo!()
                }
            

                for project in projects {
                    // get users of each project
                    if let Some(users) = get_all_elements::<User>(&state.web_client, state.token.clone().unwrap_or_default(), format!("{}{}{}{}", state.gitlab_endpoint, "/projects/" , project.id, "/users")).await {
                        let data = GitlabData::Users(users.clone());
                        match send(data, client_ref.clone(), state.registration_id.clone(), USERS_QUEUE_NAME.to_string()) {
                            Ok(_) => {
                                debug!("Successfully sent user data");
                            } Err(e) => todo!()
                        }
                    }
                    
    
                    //get runners
                    if let Some(runners) = get_all_elements::<Runner>(&state.web_client, state.token.clone().unwrap_or_default(), format!("{}{}{}{}", state.gitlab_endpoint, "/projects/", project.id, "/runners")).await {
                        let data = GitlabData::Runners(runners.clone());
                        match send(data, client_ref.clone(), state.registration_id.clone(), RUNNERS_QUEUE_NAME.to_string()) {
                            Ok(_) => {
                                debug!("Successfully sent runner data");
                            } Err(e) => todo!()
                        }
                    }
                    
    
                    // get 20 projects pipelines
                    //TODO: Get all runs of pipelines? Make configurable (number of pipeline runs to retrieve etc)
                    // if let  Ok(pipelines) =  get_project_pipelines(&state.web_client, project.id, state.token.clone().unwrap_or_default(), state.gitlab_endpoint.clone()).await {
                    //     to_string(&pipelines).map_or_else(|e|{ warn!("{e}")}, |serialized| {
                    //         let msg = ClientMessage::PublishRequest { topic: RUNNERS_QUEUE_NAME.to_string(), payload: serialized , registration_id: Some(state.registration_id.clone()) };
                    //         client.send_message(TcpClientMessage::Send(msg)).unwrap();
            
                    //     })
                    // } else { error!("Could not get project {} pipelines {}", project.id, project.name) }
                    
                }
        }
        } else {
            warn!("Failed to locate client!");
            todo!("Do we still want to read messages if we can't send anything?");
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

