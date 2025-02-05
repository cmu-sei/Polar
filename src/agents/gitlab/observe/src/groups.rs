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

use cassini::{client::TcpClientMessage, ClientMessage, UNEXPECTED_MESSAGE_STR};
use cynic::{GraphQlResponse, QueryBuilder};
use gitlab_queries::{MultiGroupQuery, MultiGroupQueryArguments};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use rkyv::rancor::Error;

use crate::{get_all_elements, GitlabObserverArgs, GitlabObserverMessage, GitlabObserverState};
use tracing::{debug, info, warn, error};
use reqwest::Client;
use serde_json::to_string;
use common::{types::{GitlabData, ResourceLink}, GROUPS_CONSUMER_TOPIC, PROJECTS_CONSUMER_TOPIC, RUNNERS_CONSUMER_TOPIC, USER_CONSUMER_TOPIC};


use crate::BROKER_CLIENT_NAME;

pub struct GitlabGroupObserver;

#[async_trait]
impl Actor for GitlabGroupObserver {
    type Msg = GitlabObserverMessage;
    type State = GitlabObserverState;
    type Arguments = GitlabObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabObserverArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to instance");
        
        let client = Client::new();

        let state = GitlabObserverState {
            gitlab_endpoint: args.gitlab_endpoint,
            token: args.token,
            web_client: client,
            registration_id: args.registration_id
        };
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State ) ->  Result<(), ActorProcessingErr> {


        myself.send_interval(Duration::from_secs(10), || { 
            //TODO: get query arguments from config params
            //build query

            let args = MultiGroupQueryArguments { 
                search: None, sort: "name_asc".to_string(), 
                marked_for_deletion_on: None, after: None, 
                before: None, first: None, last: None
            };

            let op = MultiGroupQuery::build(args);

            // pass query in message
            GitlabObserverMessage::GetGroups(op)
        });
        
        Ok(())
    }
    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        
        match message {
            GitlabObserverMessage::GetGroups(op) => {
                debug!("Sending query: {op:?}");

                match state.web_client
                .post(state.gitlab_endpoint.clone())
                .bearer_auth(state.token.clone().unwrap_or_default())
                .json(&op)
                .send().await {
                    Ok(response) => {
                        if let Ok(deserialized) = response.json::<GraphQlResponse<MultiGroupQuery>>().await {
                            if let Some(errors) = deserialized.errors {
                                for error in errors {
                                    warn!("Received errors, {error:?}");
                                }
                            }
                            if let Some(query) = deserialized.data {
                             if let Some(connection) =  query.groups {
                                
                                let mut read_groups = Vec::new();

                                if let Some(groups) = connection.nodes {
                                    read_groups.extend(groups.into_iter().filter_map(|option| {
                                        option.map(|group| {
                                            //process group
                                            // group.group_members.as_ref().map(|connection| {
                                            //    //send user group connection to groups consumer
                                            //    let client = where_is(BROKER_CLIENT_NAME.to_string()).expect("Expected to find tcp client");
                                                        
                                            //    let data = GitlabData::GroupMembers( ResourceLink {
                                            //        resource_id: group.id.clone(),
                                            //        connection: connection.clone()
                                            //    });
                                               
                                            //    let bytes = rkyv::to_bytes::<Error>(&data).unwrap();
                                               

                                            //    let msg = ClientMessage::PublishRequest {
                                            //        topic: GROUPS_CONSUMER_TOPIC.to_string(),
                                            //        payload: bytes.to_vec(),
                                            //        registration_id: Some(state.registration_id.clone())
                                            //    };
                                               
                                            //    client.send_message(TcpClientMessage::Send(msg)).expect("Expected to send message");  
                                            // });

                                            //return 
                                            group
                                        })
                                    }))
                                }

                                info!("Observed {0} group(s)", read_groups.len());

                                let client = where_is(BROKER_CLIENT_NAME.to_string()).expect("Expected to find tcp client!");
                                
                                let bytes = rkyv::to_bytes::<Error>(&GitlabData::Groups(read_groups)).unwrap();
                                

                                let msg = ClientMessage::PublishRequest { topic: GROUPS_CONSUMER_TOPIC.to_string(), payload: bytes.to_vec(), registration_id: Some(state.registration_id.clone()) };
                                client.send_message(TcpClientMessage::Send(msg)).expect("Expected to send message to tcp client!");

                             }
                                
                            }
                        }
                    },
                    Err(e) => todo!()
                }
            
            
            }
            _ => warn!(UNEXPECTED_MESSAGE_STR)
            
        }
        

        Ok(())
    }

}


