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

use crate::{graphql_endpoint, init_observer_state, send_to_broker};
use crate::{
    BackoffReason, Command, GitlabObserverArgs, GitlabObserverMessage, GitlabObserverState,
    GITLAB_PROJECT_OBSERVER,
};
use common::types::GitlabData;
use common::PIPELINE_CONSUMER_TOPIC;
use cynic::GraphQlResponse;
use cynic::QueryBuilder;
use gitlab_queries::projects::*;
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, error, info, warn};

pub struct GitlabPipelineObserver;

#[async_trait]
impl Actor for GitlabPipelineObserver {
    type Msg = GitlabObserverMessage;
    type State = GitlabObserverState;
    type Arguments = GitlabObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabObserverArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        Ok(init_observer_state(args))
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("{myself:?} Started");
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
                    Command::GetProjectPipelines(full_path) => {
                        debug!("Getting pipelines for project: {full_path:?}");

                        let op = ProjectPipelineQuery::build(SingleProjectQueryArguments {
                            full_path: full_path.clone(),
                        });

                        debug!("Sending query: {}", op.query);

                        match state
                            .web_client
                            .post(graphql_endpoint(&state.gitlab_endpoint))
                            .bearer_auth(state.token.clone().unwrap_or_default())
                            .json(&op)
                            .send()
                            .await
                        {
                            Ok(response) => {
                                match response
                                    .json::<GraphQlResponse<ProjectPipelineQuery>>()
                                    .await
                                {
                                    Ok(deserialized) => {
                                        if let Some(errors) = deserialized.errors {
                                            let errors = errors
                                                .iter()
                                                .map(|error| error.to_string())
                                                .collect::<Vec<_>>()
                                                .join("\n");

                                            error!("Failed to query instance! {errors}");
                                            myself.stop(Some(errors))
                                        } else if let Some(resp) = deserialized.data {
                                            let mut read_pipelines = Vec::new();

                                            if let Some(project) = resp.project {
                                                match project.pipelines {
                                                    Some(connection) => {
                                                        if let Some(pipelines) = connection.nodes {
                                                            if !pipelines.is_empty() {
                                                                // Append nodes to the result list.
                                                                read_pipelines.extend(
                                                                    pipelines.into_iter().map(
                                                                        |option| {
                                                                            let pipeline =
                                                                                option.unwrap();
                                                                            pipeline
                                                                        },
                                                                    ),
                                                                );

                                                                debug!("Found {0} pipeline run(s) for project {1}", read_pipelines.len(), full_path);

                                                                let data = GitlabData::Pipelines((
                                                                    full_path.0,
                                                                    read_pipelines,
                                                                ));

                                                                return send_to_broker(
                                                                    state,
                                                                    data,
                                                                    PIPELINE_CONSUMER_TOPIC,
                                                                );
                                                            }
                                                        }
                                                    }
                                                    None => (),
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Failed to deserialize response: {e}")
                                    }
                                }
                            }
                            Err(e) => {}
                        }
                    }
                    _ => todo!(),
                }
            }
            _ => (),
        }
        Ok(())
    }
}

pub struct GitlabJobObserver;

#[async_trait]
impl Actor for GitlabJobObserver {
    type Msg = GitlabObserverMessage;
    type State = GitlabObserverState;
    type Arguments = GitlabObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabObserverArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        Ok(init_observer_state(args))
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("{myself:?} Started");
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
                    // So, this hanlder get's a little messy very quickly.
                    // GitLab's GraphQL API enforces a complexity limit, so querying deeply nested structures (like project → pipelines → jobs → artifacts) often requires splitting into smaller queries.
                    // This is simple, but keeping the code clean becomes cumbersome since cynic requires very explicit queries be made.
                    // As a result, I've split the logic such that:
                    // Our pipeline observer queries a project and returns its pipelines.
                    // Our job observer performs a narrower query: given a project, only fetch pipelines with their jobs, not full pipeline objects.
                    Command::GetPipelineJobs(full_path) => {
                        debug!("Getting pipelines for project: {full_path:?}");

                        // Gitlab's Jobs API will only return empty results for non admin users, since it'ls unlikey people will give admin level tokens
                        // we can instead rely on a project-level query and use a modified version of our pipelines query
                        let op = ProjectPipelineJobsQuery::build(SingleProjectQueryArguments {
                            full_path: full_path.clone(),
                        });

                        debug!("Sending query: {}", op.query);

                        match state
                            .web_client
                            .post(graphql_endpoint(&state.gitlab_endpoint))
                            .bearer_auth(state.token.clone().unwrap_or_default())
                            .json(&op)
                            .send()
                            .await
                        {
                            Ok(response) => {
                                match response
                                    .json::<GraphQlResponse<ProjectPipelineJobsQuery>>()
                                    .await
                                {
                                    // Handle successful data response
                                    Ok(deserialized) => {
                                        if let Some(errors) = deserialized.errors {
                                            let errors = errors
                                                .iter()
                                                .map(|error| error.to_string())
                                                .collect::<Vec<_>>()
                                                .join("\n");

                                            error!("Failed to query instance! {errors}");
                                        }
                                        // sift through and dig down to find the jobs list.
                                        else if let Some(resp) = deserialized.data {
                                            if let Some(project) = resp.project {
                                                match project.pipelines {
                                                    Some(connection) => {
                                                        if let Some(pipelines) = connection.nodes {
                                                            if !pipelines.is_empty() {
                                                                for p in pipelines {
                                                                    match p {
                                                                        Some(pipeline) => {
                                                                            if let Some(conn) =
                                                                                pipeline.jobs
                                                                            {
                                                                                // If pipeline has jobs, extract them
                                                                                if let Some(jobs) =
                                                                                    conn.nodes
                                                                                {
                                                                                    let mut read_jobs: Vec<GitlabCiJob> = Vec::new();

                                                                                    // the meat of everything happens here
                                                                                    // wrap up job and add it to the vec

                                                                                    read_jobs.extend(jobs.into_iter().map(|option| {
                                                                                        let job = option.unwrap();
                                                                                        job
                                                                                    }));

                                                                                    let data = GitlabData::Jobs((pipeline.id.0, read_jobs));

                                                                                    if let Err(e) = send_to_broker(state, data, PIPELINE_CONSUMER_TOPIC) { return Err(e) }
                                                                                }
                                                                            }
                                                                        }
                                                                        None => (),
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                    None => (),
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Failed to deserialize response: {e}")
                                    }
                                }
                            }
                            Err(e) => {
                                error!("{e}");
                                match where_is(GITLAB_PROJECT_OBSERVER.to_string()) {
                                    Some(observer) => {
                                        observer
                                            .send_message(GitlabObserverMessage::Backoff(
                                                BackoffReason::GitlabUnreachable(e.to_string()),
                                            ))
                                            .unwrap();
                                    }
                                    None => myself.stop(None),
                                }
                            }
                        }
                    }
                    _ => todo!(),
                }
            }
            _ => (),
        }
        Ok(())
    }
}
