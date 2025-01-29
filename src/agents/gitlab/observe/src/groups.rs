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



use cassini::{client::TcpClientMessage};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};

use crate::{get_all_elements, GitlabObserverArgs, GitlabObserverState};
use tracing::{debug, info, warn, error};
use reqwest::Client;
use serde_json::to_string;
use common::{types::{GitlabData, Project, ResourceLink, Runner, User, UserGroup}, GROUPS_QUEUE_NAME, PROJECTS_QUEUE_NAME, RUNNERS_QUEUE_NAME, USERS_QUEUE_NAME};


use crate::BROKER_CLIENT_NAME;

pub struct GitlabGroupObserver;

#[async_trait]
impl Actor for GitlabGroupObserver {
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
                    web_client: client,
                    registration_id: args.registration_id
                };
                Ok(state)
            }
            Err(e) => {   
                error!("{e}");
                todo!()
        
            }
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State ) ->  Result<(), ActorProcessingErr> {

        //TODO: use client in state to pull gitlab user data
        // let users: Vec<> = get_all_elements(&state.state.web_client, state.token.clone().unwrap_or_default(), format!("{}{}", state.gitlab_endpoint, "/users")).await.unwrap();
        
        //forwrard to client
        // if let Some(client) = where_is(BROKER_CLIENT_NAME.to_string()) {
        //     let client_ref: ActorRef<TcpClientMessage> = ActorRef::from(client);

        //     if let Some(groups) = get_all_elements::<UserGroup>(&state.web_client, state.token.clone().unwrap_or_default(), format!("{}{}", state.gitlab_endpoint, "/groups")).await {
        //         let data = GitlabData::Groups(groups.clone());
        //         if let Err(e) = send(data, client_ref.clone(), state.registration_id.clone(), GROUPS_QUEUE_NAME.to_string()){
        //             error!("{e}");
        //             todo!();
        //         }
        //         for group in groups {
        //             if let Some(users) = get_all_elements::<User>(&state.web_client, state.token.clone().unwrap_or_default(), format!("{}{}{}{}", state.gitlab_endpoint, "/groups/" , group.id, "/members")).await {
        //                 let data = GitlabData::GroupMembers(ResourceLink {
        //                     resource_id: group.id,
        //                     resource_vec: users
        //                 });
        //                 if let Err(e) = send(data, client_ref.clone(), state.registration_id.clone(), GROUPS_QUEUE_NAME.to_string()){
        //                     error!("{e}");
        //                     todo!()
        //                 }
                        
        //             } 
        //             if let Some(runners) = get_all_elements::<Runner>(&state.web_client, state.token.clone().unwrap_or_default(), format!("{}{}{}{}", state.gitlab_endpoint, "/groups/", group.id, "/runners")).await {
        //                 let data = GitlabData::GroupRunners(ResourceLink {
        //                     resource_id: group.id,
        //                     resource_vec: runners
        //                 });
        //                 if let Err(e) = send(data, client_ref.clone(), state.registration_id.clone(), GROUPS_QUEUE_NAME.to_string()){
        //                     error!("{e}");
        //                     todo!()
        //                 }
                        
        //             }
        //             if let Some(projects) = get_all_elements::<Project>(&state.web_client, state.token.clone().unwrap_or_default(), format!("{}{}{}{}", state.gitlab_endpoint, "/groups/", group.id, "/projects")).await {
        //                 let data = GitlabData::GroupProjects(ResourceLink {
        //                     resource_id: group.id,
        //                     resource_vec: projects
        //                 });
        //                 if let Err(e) = send(data, client_ref.clone(), state.registration_id.clone(), GROUPS_QUEUE_NAME.to_string()){
        //                     error!("{e}");
        //                     todo!()
        //                 }
 
        //             }

        //         }
        //     }
        // }
        
        // myself.stop(Some("FINISHED".to_string()));
        
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


