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

use crate::graphql_endpoint;
use crate::{
    init_observer_state, send_to_broker, BackoffReason, Command, GitlabObserverArgs,
    GitlabObserverMessage, GitlabObserverState, MESSAGE_FORWARDING_FAILED,
};
use common::types::{GitlabData, GitlabPackageFile};
use common::REPOSITORY_CONSUMER_TOPIC;
use cynic::{GraphQlResponse, QueryBuilder};
use gitlab_queries::projects::{
    ContainerRepository, ContainerRepositoryDetailsArgs, ContainerRepositoryDetailsQuery,
    ContainerRepositoryTag, Package, ProjectContainerRepositoriesQuery, ProjectPackagesQuery,
    SingleProjectQueryArguments,
};
use gitlab_schema::ContainerRepositoryID;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use serde_json::from_str;
use tracing::{debug, error, info, warn};

pub struct GitlabRepositoryObserver;

impl GitlabRepositoryObserver {
    /// helper to quickly scrape package files from gitlab's REST API and pass them t0 the consumer side.
    async fn get_gitlab_package_files(
        state: &mut GitlabObserverState,
        project_gid: &str,
        package_gid: &str,
    ) -> Result<(), ActorProcessingErr> {
        // parse and strip ids
        let project_id = crate::extract_gitlab_id(project_gid).unwrap();
        let package_id = crate::extract_gitlab_id(package_gid).unwrap();

        let url = format!(
            "{}/api/v4/projects/{}/packages/{}/package_files",
            state.gitlab_endpoint, project_id, package_id
        );
        debug!("GET {url}");

        let res = state
            .web_client
            .get(&url)
            .bearer_auth(state.token.clone().unwrap_or_default())
            .header("Accept", "application/json")
            .send()
            .await
            .unwrap();

        let files: Vec<GitlabPackageFile> = res.json().await.unwrap();

        let data = GitlabData::PackageFiles((package_gid.to_string(), files));

        send_to_broker(state, data, REPOSITORY_CONSUMER_TOPIC)
    }

    async fn get_repository_tags(
        web_client: reqwest::Client,
        full_path: ContainerRepositoryID,
        gitlab_token: String,
        gitlab_endpoint: String,
    ) -> Result<Vec<ContainerRepositoryTag>, ActorProcessingErr> {
        let op = ContainerRepositoryDetailsQuery::build(ContainerRepositoryDetailsArgs {
            id: full_path.clone(),
        });
        info!("Getting tags for path: {full_path}");
        debug!("{}", op.query);

        let response = web_client
            .post(gitlab_endpoint)
            .bearer_auth(gitlab_token)
            .json(&op)
            .send()
            .await?;

        let response_text = response.text().await?;

        let deserialized_response =
            from_str::<GraphQlResponse<ContainerRepositoryDetailsQuery>>(&response_text)?;

        if let Some(errors) = deserialized_response.errors {
            let combined = errors
                .into_iter()
                .map(|e| format!("{e:?}"))
                .collect::<Vec<_>>()
                .join("\n");

            warn!("Received GraphQL errors:\n{combined}");
            // Return or propagate as needed, e.g.:
            return Err(ActorProcessingErr::from(combined));
        } else if let Some(data) = deserialized_response.data {
            if let Some(details) = data.container_repository {
                match details.tags {
                    Some(connection) => {
                        match connection.nodes {
                            Some(tags) => {
                                let mut read_tags = Vec::new();

                                for option in tags {
                                    if let Some(tag) = option {
                                        read_tags.push(tag);
                                    }
                                }

                                // return list
                                Ok(read_tags)
                            }
                            None => Err(ActorProcessingErr::from("No data found")),
                        }
                    }
                    None => Err(ActorProcessingErr::from("No data found")),
                }
            } else {
                Err(ActorProcessingErr::from("No data found"))
            }
        } else {
            Err(ActorProcessingErr::from("No data found."))
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
        _myself: ActorRef<Self::Msg>,
        args: GitlabObserverArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(init_observer_state(args))
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("{myself:?} started");
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
                        let op =
                            ProjectContainerRepositoriesQuery::build(SingleProjectQueryArguments {
                                full_path: full_path.clone(),
                            });

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
                                    .json::<GraphQlResponse<ProjectContainerRepositoriesQuery>>()
                                    .await
                                {
                                    Ok(deserialized_response) => {
                                        if let Some(errors) = deserialized_response.errors {
                                            let errors = errors
                                                .iter()
                                                .map(|error| error.to_string())
                                                .collect::<Vec<_>>()
                                                .join("\n");

                                            error!("Failed to query instance! {errors}");
                                            myself.stop(Some(errors))
                                        } else if let Some(data) = deserialized_response.data {
                                            if let Some(project) = data.project {
                                                if let Some(connection) =
                                                    project.container_repositories
                                                {
                                                    match connection.nodes {
                                                        Some(repositories) => {
                                                            let mut read_repositories: Vec<
                                                                ContainerRepository,
                                                            > = Vec::new();

                                                            for option in repositories {
                                                                match option {
                                                                    Some(repository) => {
                                                                        // try to get image tags from that repo
                                                                        let id =
                                                                            ContainerRepositoryID(
                                                                                repository
                                                                                    .id
                                                                                    .0
                                                                                    .clone(),
                                                                            );
                                                                        let result = GitlabRepositoryObserver::get_repository_tags(
                                                                            state.web_client.clone(),
                                                                            id,
                                                                            state.token.clone().unwrap_or_default(),
                                                                            graphql_endpoint(&state.gitlab_endpoint))
                                                                            .await;

                                                                        match result {
                                                                            Ok(tags) => {
                                                                                //send tag data
                                                                                if !tags.is_empty()
                                                                                {
                                                                                    let data = GitlabData::ContainerRepositoryTags((full_path.clone().to_string(), tags));
                                                                                    if let Err(e) = send_to_broker(state, data, REPOSITORY_CONSUMER_TOPIC) { return Err(e) }
                                                                                }
                                                                            }
                                                                            Err(e) => warn!("{e}"),
                                                                        };

                                                                        read_repositories
                                                                            .push(repository);
                                                                    }
                                                                    None => (),
                                                                }
                                                            }

                                                            // send off repository data
                                                            if !read_repositories.is_empty() {
                                                                let data = GitlabData::ProjectContainerRepositories((full_path.to_string(), read_repositories));

                                                                if let Err(e) = send_to_broker(
                                                                    state,
                                                                    data,
                                                                    REPOSITORY_CONSUMER_TOPIC,
                                                                ) {
                                                                    return Err(e);
                                                                }
                                                            }
                                                        }
                                                        None => (),
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        error!("Failed to deserialize response from server: {e}")
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Error observing data: {e}")
                            }
                        }
                    }
                    Command::GetProjectPackages(full_path) => {
                        let op = ProjectPackagesQuery::build(SingleProjectQueryArguments {
                            full_path: full_path.clone(),
                        });

                        debug!("{}", op.query);

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
                                    .json::<GraphQlResponse<ProjectPackagesQuery>>()
                                    .await
                                {
                                    Ok(deserialized_response) => {
                                        if let Some(errors) = deserialized_response.errors {
                                            let errors = errors
                                                .iter()
                                                .map(|error| error.to_string())
                                                .collect::<Vec<_>>()
                                                .join("\n");

                                            error!("Failed to query instance! {errors}");
                                            myself.stop(Some(errors))
                                        } else if let Some(data) = deserialized_response.data {
                                            if let Some(project) = data.project {
                                                if let Some(connection) = project.packages {
                                                    match connection.nodes {
                                                        Some(packages) => {
                                                            if !packages.is_empty() {
                                                                let mut read_packages: Vec<
                                                                    Package,
                                                                > = Vec::new();

                                                                for package in packages {
                                                                    let pkg = package.unwrap();
                                                                    // get package file metadtadta
                                                                    GitlabRepositoryObserver::get_gitlab_package_files(state, &project.id.0, &pkg.id.0).await.unwrap();
                                                                    read_packages.push(pkg);
                                                                }

                                                                let data =
                                                                    GitlabData::ProjectPackages((
                                                                        full_path.to_string(),
                                                                        read_packages,
                                                                    ));

                                                                if let Err(e) = send_to_broker(
                                                                    state,
                                                                    data,
                                                                    REPOSITORY_CONSUMER_TOPIC,
                                                                ) {
                                                                    return Err(e);
                                                                }
                                                            }
                                                        }
                                                        None => (),
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => myself
                                        .send_message(GitlabObserverMessage::Backoff(
                                            BackoffReason::GitlabUnreachable(e.to_string()),
                                        ))
                                        .expect(MESSAGE_FORWARDING_FAILED),
                                }
                            }
                            Err(e) => error!("{e}"),
                        }
                    }
                    _ => (),
                }
            }
            _ => (), // this observer only acts when it is told to, it has no need to
        }
        Ok(())
    }
}
