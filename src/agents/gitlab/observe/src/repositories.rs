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

use crate::{
    Command, GitlabObserverArgs, GitlabObserverMessage, GitlabObserverState, BROKER_CLIENT_NAME
};
use cassini::{client::TcpClientMessage, ClientMessage};
use gitlab_queries::projects::{ContainerRepository, ContainerRepositoryDetailsArgs, ContainerRepositoryDetailsQuery, ContainerRepositoryTag, Package, ProjectContainerRepositoriesQuery, ProjectPackagesQuery, SingleProjectQueryArguments};
use gitlab_schema::ContainerRepositoryID;
use ractor::concurrency::Duration;
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use common::types::{GitlabData};
use common::REPOSITORY_CONSUMER_TOPIC;
use tracing::{debug, error, warn};
use cynic::{GraphQlResponse, QueryBuilder};

pub struct GitlabRepositoryObserver;

impl GitlabRepositoryObserver {

    async fn get_repository_tags(web_client: reqwest::Client, full_path: ContainerRepositoryID, gitlab_token: String, gitlab_endpoint: String) -> Result<Vec<ContainerRepositoryTag>, String> {
         
        let op = ContainerRepositoryDetailsQuery::build(ContainerRepositoryDetailsArgs {
            id: full_path.clone()
        });
        debug!("Getting tags for path: {full_path}");
        debug!("{}", op.query);
        
        match web_client
        .post(gitlab_endpoint)
        .bearer_auth(gitlab_token)
        .json(&op)
        .send()
        .await {
            Ok(response) => {
                match response.json::<GraphQlResponse<ContainerRepositoryDetailsQuery>>().await {
                    Ok(deserialized_response) => {
                        if let Some(errors) = deserialized_response.errors {
                            let combined = errors
                                .into_iter()
                                .map(|e| format!("{e:?}"))
                                .collect::<Vec<_>>()
                                .join("\n");
                            
                            warn!("Received GraphQL errors:\n{combined}");
                            // Return or propagate as needed, e.g.:
                            Err(combined)
                        }
                        else if let Some(data) = deserialized_response.data {
                            if let Some(details) = data.container_repository {
                                match details.tags {
                                    Some(connection) => {                  
                                        match connection.nodes {
                                            Some(tags) => {
                                                let mut read_tags = Vec::new();

                                                for option in tags {
                                                    if let Some(tag) = option { read_tags.push(tag); }
                                                }

                                                // return list
                                                Ok(read_tags)
                                            }
                                            None =>  Err(String::from("No data found"))
                                        }
                                    }
                                    None => Err(String::from("No data found"))
                                }
                            } else {  Err(String::from("No data found")) }
                        } else {  Err(String::from("No data found.")) }
                    }
                    Err(e) => Err(e.to_string())
                } 
            }
            Err(e) => Err(e.to_string())
        }
    }
}


#[async_trait]
impl Actor for GitlabRepositoryObserver {
    type Msg = GitlabObserverMessage;
    type State = GitlabObserverState;
    type Arguments = GitlabObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabObserverArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        
        let state = GitlabObserverState::new(
            args.gitlab_endpoint,
            args.token,
            args.web_client,
            args.registration_id, 
            Duration::from_secs(args.base_interval),    
            Duration::from_secs(args.max_backoff),
        );
        
        Ok(state)
    }

    async fn post_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            GitlabObserverMessage::Tick(command) => {
                match command {
                    Command::GetProjectContainerRepositories(full_path) => {
                        
                        let op = ProjectContainerRepositoriesQuery::build(SingleProjectQueryArguments {
                            full_path: full_path.clone()
                        });
        
                        match state
                        .web_client
                        .post(state.gitlab_endpoint.clone())
                        .bearer_auth(state.token.clone().unwrap_or_default())
                        .json(&op)
                        .send()
                        .await {
                            Ok(response) => {
                                match response.json::<GraphQlResponse<ProjectContainerRepositoriesQuery>>().await {
                                    Ok(deserialized_response) => {
                                        if let Some(errors) = deserialized_response.errors {
                                            let errors = errors
                                            .iter()
                                            .map(|error| { error.to_string() })
                                            .collect::<Vec<_>>()
                                            .join("\n");
                    
                                            error!("Failed to query instance! {errors}");
                                            myself.stop(Some(errors))
                                        }
                                        else if let Some(data) = deserialized_response.data {
                                            
                                            if let Some(project) = data.project {
                                                
                                                if let Some(connection) = project.container_repositories {
                                                    match connection.nodes {
                                                        Some(repositories) => {
                                                            let mut read_repositories: Vec<ContainerRepository> = Vec::new();
        
                                                            for option in repositories {
                                                                match option {
                                                                    Some(repository) => {
                                                                        // try to get image tags from that repo
                                                                        let id = ContainerRepositoryID(repository.id.0.clone());
                                                                        let result = GitlabRepositoryObserver::get_repository_tags(
                                                                            state.web_client.clone(),
                                                                            id,
                                                                            state.token.clone().unwrap_or_default(),
                                                                            state.gitlab_endpoint.clone())
                                                                            .await;
        
                                                                        match result {
                                                                            Ok(tags) => {
                                                                                //send tag data
                                                                                if !tags.is_empty() {
                                                                                    match where_is(BROKER_CLIENT_NAME.to_string()) {
                                                                                        Some(tcp_client) => {
                                                                                            
                                                                                            let tag_message = GitlabData::ContainerRepositoryTags((full_path.clone().to_string(), tags));
                                                                                            
                                                                                            match rkyv::to_bytes::<rkyv::rancor::Error>(&tag_message) {
                                                                                                Ok(bytes) => {
                                                                                                    let msg = ClientMessage::PublishRequest {
                                                                                                        topic: REPOSITORY_CONSUMER_TOPIC.to_string(),
                                                                                                        payload: bytes.to_vec(),
                                                                                                        registration_id: Some(state.registration_id.clone()),
                                                                                                    };
                                                                                                    tcp_client
                                                                                                        .send_message(TcpClientMessage::Send(msg))
                                                                                                        .expect("Expected to send message");
                                                                                                }
                                                                                                Err(e) => error!("Failed to serialize data. {e}")
                                                                                            }
                                                                                        }
                                                                                        None => {
                                                                                            warn!("Couldn't find client to send data, stopping.");
                                                                                            myself.stop(None)
                                                                                        }
                                                                                    } 
                                                                                }
                                                                            },
                                                                            Err(e) => warn!("{e}")
                                                                        }
                                                                        
                                                                        read_repositories.push(repository);
                                                                    }
                                                                    None => ()
                                                                }
                                                            }
        
                                                            // send off repository data
        
                                                            if !read_repositories.is_empty() {
                                                                match where_is(BROKER_CLIENT_NAME.to_string()) {
                                                                    Some(tcp_client) => {
                                                                        
                                                                        let repo_data = GitlabData::ProjectContainerRepositories((full_path.to_string(), read_repositories));
                                                                        
                                                                        match rkyv::to_bytes::<rkyv::rancor::Error>(&repo_data) {
                                                                            Ok(bytes) => {
                                                                                let msg = ClientMessage::PublishRequest {
                                                                                    topic: REPOSITORY_CONSUMER_TOPIC.to_string(),
                                                                                    payload: bytes.to_vec(),
                                                                                    registration_id: Some(state.registration_id.clone()),
                                                                                };
                                                                                tcp_client
                                                                                    .send_message(TcpClientMessage::Send(msg))
                                                                                    .expect("Expected to send message");
                                                                            }
                                                                            Err(e) => error!("Failed to serialize data. {e}")
                                                                        }
                                                                        
                                                                    }
                                                                    None => {
                                                                        warn!("Couldn't find client to send data, stopping.");
                                                                        myself.stop(None)
                                                                    }
                                                                }
                                                            }
                                                        }
                                                        None => ()
                                                    }
                                                }
                                            }
                                        }
                                        
                                    }
                                    Err(e) => error!("Failed to deserialize response from server: {e}")
                                }
                            }
                            Err(e) => {
                                warn!("Error observing data: {e}")
                            }
                        }
                        
                    }
                    Command::GetProjectPackages(full_path) => {
                        let op = ProjectPackagesQuery::build(SingleProjectQueryArguments{
                            full_path: full_path.clone()
                        });
                        
                        debug!("{}", op.query);
        
                        match state
                        .web_client
                        .post(state.gitlab_endpoint.clone())
                        .bearer_auth(state.token.clone().unwrap_or_default())
                        .json(&op)
                        .send()
                        .await {
                            Ok(response) => {
        
                                match response.json::<GraphQlResponse<ProjectPackagesQuery>>().await {
                                    Ok(deserialized_response) => {
        
                                        if let Some(errors) = deserialized_response.errors {
                                            let errors = errors
                                            .iter()
                                            .map(|error| { error.to_string() })
                                            .collect::<Vec<_>>()
                                            .join("\n");
                    
                                            error!("Failed to query instance! {errors}");
                                            myself.stop(Some(errors))
                                        }
                                        else if let Some(data) = deserialized_response.data {
                                            
                                            if let Some(project) = data.project  {
                                                
                                                if let Some(connection) = project.packages {
        
                                                    match connection.nodes {
                                                        Some(packages) => {
                                                            
                                                            if !packages.is_empty() {
                                                                let mut read_packages: Vec<Package> = Vec::new();
        
                                                                read_packages.extend(packages.into_iter().map(|option| { option.unwrap() }));
        
                                                                debug!("Found {} package(s) for project {full_path}", read_packages.len());
        
                                                                    
                                                                match where_is(BROKER_CLIENT_NAME.to_string()) {
                                                                    Some(client) => {
                                                                        let data = GitlabData::ProjectPackages((full_path.to_string(), read_packages));
                                                                        // Serializing is as easy as a single function call
                                                                        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&data).unwrap();
                                                                        
                                                                        let msg = ClientMessage::PublishRequest {
                                                                            topic: REPOSITORY_CONSUMER_TOPIC.to_string(),
                                                                            payload: bytes.to_vec(),
                                                                            registration_id: Some(
                                                                                state.registration_id.clone(),
                                                                            ),
                                                                        };
                        
                                                                        client
                                                                            .send_message(TcpClientMessage::Send(msg))
                                                                            .expect("Expected to send message");
                                                                    }
                                                                    None => {
                                                                        let err_msg = "Failed to locate tcp client";
                                                                        error!("{err_msg}");
                                                                    }
                                                                }
                                                            
                                                                
                                                            }
                                                        }
                                                        None => ()
                                                    }
                                                }
                                            }
                                        }
                                    }   
                                    Err(e) => todo!()
                                }
                            },
                            Err(e) => error!("{e}")
                        }
        
                    }
                
                    _ => (),
                }
            }
            _ => () // this observer only acts when it is told to, it has no need to

            }
        Ok(())
    }
}
