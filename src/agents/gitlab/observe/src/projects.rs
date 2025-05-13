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
use crate::{handle_backoff, BackoffReason, Command, GitlabObserverArgs, GitlabObserverMessage, GitlabObserverState, BACKOFF_RECEIVED_LOG, BROKER_CLIENT_NAME, GITLAB_JOBS_OBSERVER, GITLAB_PIPELINE_OBSERVER, GITLAB_REPOSITORY_OBSERVER, MESSAGE_FORWARDING_FAILED};
use cassini::{client::TcpClientMessage, ClientMessage};
use common::types::GitlabData;
use common::PROJECTS_CONSUMER_TOPIC;
use cynic::{GraphQlResponse, QueryBuilder};
use gitlab_queries::projects::{MultiProjectQuery, MultiProjectQueryArguments};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use rkyv::rancor::Error;
use std::time::Duration;
use tracing::{debug, error, info, warn};

pub struct GitlabProjectObserver;

impl GitlabProjectObserver {

    fn observe(myself: ActorRef<GitlabObserverMessage>, state: &mut GitlabObserverState, duration: Duration) {
        info!("Observing every {} seconds", state.backoff_interval.as_secs());

            let handle = myself.send_interval(state.backoff_interval, || {
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
            let command = Command::GetProjects(op);
            
            GitlabObserverMessage::Tick(command)
        }).abort_handle();

        state.task_handle = Some(handle);
    }
    
}
#[async_trait]
impl Actor for GitlabProjectObserver {
    type Msg = GitlabObserverMessage;
    type State = GitlabObserverState;
    type Arguments = GitlabObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabObserverArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to instance");

        let state = GitlabObserverState::new(
            args.gitlab_endpoint, 
            args.token, 
            args.web_client, 
            args.registration_id, 
            Duration::from_secs(args.base_interval), 
            Duration::from_secs(args.max_backoff));
        Ok(state)
    }
    
    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {

        GitlabProjectObserver::observe(myself, state, state.base_interval);
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

                    Command::GetProjects(op) => {
                        debug!("{}", op.query);
                        match state
                            .web_client
                            .post(state.gitlab_endpoint.clone())
                            .bearer_auth(state.token.clone().unwrap_or_default())
                            .json(&op)
                            .send()
                            .await
                        {
                            Ok(response) => {
                                match response.json::<GraphQlResponse<MultiProjectQuery>>().await {
                                    Ok(deserialized) => {
                                        if let Some(errors) = deserialized.errors {
                                            let errors = errors
                                            .iter()
                                            .map(|error| { error.to_string() })
                                            .collect::<Vec<_>>()
                                            .join("\n");
                    
                                            error!("Failed to query instance! {errors}");
    
                                            if let Err(e) = myself.send_message(GitlabObserverMessage::Backoff(BackoffReason::GraphqlError(errors))) {        
                                                error!("{e}");
                                                myself.stop(Some(e.to_string()))
                                            }
                                        }
                                        else if let Some(query) = deserialized.data {
                                            let connection = query.projects.unwrap();
        
                                            debug!("Found {count} project(s)", count = connection.count);
        
                                            let mut read_projects = Vec::new();
        
                                            if let Some(projects) = connection.nodes {
                                                // Append nodes to the result list.
                                                read_projects.extend(projects.into_iter().map(|option| {
                                                    let project = option.unwrap();
        
                                                    // get this project's pipeline runs
                                                    let command = Command::GetProjectPipelines(project.full_path.clone());
                                                    if let Some(pipeline_observer) = where_is(GITLAB_PIPELINE_OBSERVER.to_string()) {
                                                        pipeline_observer
                                                            .send_message(GitlabObserverMessage::Tick(command))
                                                            .expect("Expected to send message to pipeline observer");
                                                    }
        
                                                    // get jobs from this project's pipelines
                                                    let command = Command::GetPipelineJobs(project.full_path.clone());
                                                    if let Some(jobs_observer) = where_is(GITLAB_JOBS_OBSERVER.to_string()) {
                                                        jobs_observer
                                                            .send_message(GitlabObserverMessage::Tick(command))
                                                            .expect("Expected to send message to pipeline observer");
                                                    }
        
                                                    
                                                    if let Some(repository_observer) = where_is(GITLAB_REPOSITORY_OBSERVER.to_string()) {
                                                        let command = Command::GetProjectContainerRepositories(project.full_path.clone());
        
                                                        repository_observer
                                                            .send_message(GitlabObserverMessage::Tick(command))
                                                            .expect("Expected to send message to the registry observer");
        
                                                        let command = Command::GetProjectPackages(project.full_path.clone());
        
                                                        repository_observer
                                                            .send_message(GitlabObserverMessage::Tick(command))
                                                            .expect("Expected to send message to the registry observer");
        
                                                    }
                                                    
                                                    //return the projects
                                                    project
                                                }));
                                            }
                                            
                                            let tcp_client = where_is(BROKER_CLIENT_NAME.to_string())
                                                .expect("Expected to find client");
        
                                            let data = GitlabData::Projects(read_projects.clone());
                                            
                                            let bytes = rkyv::to_bytes::<Error>(&data).unwrap();
        
                                            let msg = ClientMessage::PublishRequest {
                                                topic: PROJECTS_CONSUMER_TOPIC.to_string(),
                                                payload: bytes.to_vec(),
                                                registration_id: Some(state.registration_id.clone()),
                                            };
                                            tcp_client
                                                .send_message(TcpClientMessage::Send(msg))
                                                .expect("Expected to send message");
        
        
                                        }
                                    }
                                    Err(e) => myself.send_message(GitlabObserverMessage::Backoff(BackoffReason::FatalError(e.to_string()))).expect(MESSAGE_FORWARDING_FAILED)
                                }
                            }
                            Err(e) => myself.send_message(GitlabObserverMessage::Backoff(BackoffReason::GitlabUnreachable(e.to_string()))).expect(MESSAGE_FORWARDING_FAILED)
                        }
                    }
                    _ => (),
                }
            }
            GitlabObserverMessage::Backoff(reason) => {
                // cancel old event loop and start a new one with updated state, if observer hasn't stopped
                if let Some(handle) = &state.task_handle {
                    handle.abort();
                    // start new loop
                    match handle_backoff(state, reason) {
                        Ok(duration) => {
                            GitlabProjectObserver::observe(myself, state, duration);
                        }
                        Err(e) => myself.stop(Some(e.to_string()))
                    }   
                }
            }
        }
        Ok(())
    }
}
