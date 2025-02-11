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
use crate::{GitlabObserverArgs, GitlabObserverMessage, GitlabObserverState, BROKER_CLIENT_NAME};
use cassini::{client::TcpClientMessage, ClientMessage};
use common::PROJECTS_CONSUMER_TOPIC;
use cynic::{GraphQlResponse, QueryBuilder};
use gitlab_queries::{MultiProjectQuery, MultiProjectQueryArguments};
use rkyv::rancor::Error;
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use reqwest::Client;
use common::types::GitlabData;
use std::time::Duration;
use tracing::debug;


pub struct GitlabProjectObserver;

#[async_trait]
impl Actor for GitlabProjectObserver {
    type Msg = GitlabObserverMessage;
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
        _state: &mut Self::State ) ->  Result<(), ActorProcessingErr> {

        

        // get projects on interval
        // TODO: initiate loops based off of provided schedule, resources should be retrieved on some given interval
        myself.send_interval(Duration::from_secs(10), || { 
            //TODO: get query arguments from config params
            //build query
            let op = MultiProjectQuery::build(MultiProjectQueryArguments {
                membership: None,
                search: None,
                search_namespaces: None,
                topics: None,
                personal: None, 
                sort: "name_asc".to_string(),
                ids: None,
                full_paths: None, 
                with_issues_enabled: None,
                with_merge_requests_enabled: None, 
                aimed_for_deletion: None,
                include_hidden: None,
                marked_for_deletion_on: None,
                after: None,
                before: None,
                first: None,
                last: None,
            });

            // pass query in message
            GitlabObserverMessage::GetProjects(op) 
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
            GitlabObserverMessage::GetProjects(op) => {
                match state.web_client
                .post(state.gitlab_endpoint.clone())
                .bearer_auth(state.token.clone().unwrap_or_default())
                .json(&op)
                .send().await {
                    Ok(response) => {
                        
                        match response.json::<GraphQlResponse<MultiProjectQuery>>().await {
                            Ok(deserialized) => {
                                if let Some(query) = deserialized.data {
                                    let connection = query.projects.unwrap();

                                    debug!("Found {count} project(s)", count=connection.count);

                                    let mut read_projects = Vec::new();

                                    if let Some(projects) = connection.nodes {
                                        // Append nodes to the result list.
                                        read_projects.extend(projects.into_iter().map(|option| {
                                            //TODO: Get project runners, issues, etc.
                                            option.unwrap()
                                        }));
                                    }
                                    
                                    let tcp_client = where_is(BROKER_CLIENT_NAME.to_string()).expect("Expected to find client");

                                    let data = GitlabData::Projects(read_projects);
                                    // Serializing is as easy as a single function call
                                    let bytes = rkyv::to_bytes::<Error>(&data).unwrap();
                                    
                                    let msg = ClientMessage::PublishRequest { topic: PROJECTS_CONSUMER_TOPIC.to_string(), payload: bytes.to_vec(), registration_id: Some(state.registration_id.clone()) };
                                    tcp_client.send_message(TcpClientMessage::Send(msg)).expect("Expected to send message");
                                }
                            }
                            Err(e) => todo!()
                        }
                    },
                    Err(e) => todo!()
                }
            }
            _ => todo!()
        }
        Ok(())
    }
}

