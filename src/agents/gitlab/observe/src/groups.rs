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

use ractor::concurrency::Duration;

use crate::{
    graphql_endpoint, handle_backoff, BackoffReason, Command, GitlabObserverArgs,
    GitlabObserverMessage, GitlabObserverState, MESSAGE_FORWARDING_FAILED,
};
use cassini::{client::TcpClientMessage, ClientMessage, UNEXPECTED_MESSAGE_STR};
use common::{
    types::{GitlabData, ResourceLink},
    GROUPS_CONSUMER_TOPIC,
};
use cynic::{GraphQlResponse, Operation, QueryBuilder};
use gitlab_queries::groups::{
    AllGroupsQuery, GroupMembersQuery, GroupPathVariable, GroupProjectsQuery, GroupRunnersQuery,
    MultiGroupQueryArguments,
};

use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use reqwest::Client;

use tracing::{debug, error, info, warn};

use crate::BROKER_CLIENT_NAME;

pub struct GitlabGroupObserver;

impl GitlabGroupObserver {
    /// Helper fn to start a new event loop and update state with a new abort handle for the task.
    /// This function is needed because ractor agents maintain concurrency internally. Every time we call the send_interval() function, the duration
    /// is copied in and used to create a new loop. So it's more prudent to simply kill and replace that thread than it is to
    /// loop for a dynamic amount of time
    fn observe(
        myself: ActorRef<GitlabObserverMessage>,
        state: &mut GitlabObserverState,
        duration: Duration,
    ) {
        info!("Observing every {} seconds", duration.as_secs());
        let handle = myself
            .send_interval(duration, || {
                // build query
                let args = MultiGroupQueryArguments {
                    search: None,
                    sort: "name_asc".to_string(),
                    marked_for_deletion_on: None,
                    after: None,
                    before: None,
                    first: None,
                    last: None,
                };

                let op = AllGroupsQuery::build(args);

                // pass query in message
                let command = Command::GetGroups(op);
                //execute query
                GitlabObserverMessage::Tick(command)
            })
            .abort_handle();

        state.task_handle = Some(handle);
    }

    // helper to get a group memberships given a query operation
    async fn get_group_members(
        client: Client,
        token: String,
        registration_id: String,
        endpoint: String,
        op: Operation<GroupMembersQuery, GroupPathVariable>,
    ) -> Result<(), String> {
        debug!("Sending query: {:?}", op.query);

        match client
            .post(endpoint)
            .bearer_auth(token)
            .json(&op)
            .send()
            .await
        {
            Ok(response) => match response.json::<GraphQlResponse<GroupMembersQuery>>().await {
                Ok(deserialized) => {
                    if let Some(errors) = deserialized.errors {
                        let errors = errors
                            .iter()
                            .map(|error| error.to_string())
                            .collect::<Vec<_>>()
                            .join("\n");

                        error!("Failed to query instance! {errors}");
                        return Err(errors);
                    }

                    let query = deserialized
                        .data
                        .expect("Expected there to be something here");
                    query.group.map(|group| {
                        let conn = group.group_members.unwrap();

                        let client = where_is(BROKER_CLIENT_NAME.to_string())
                            .expect("Expected to find tcp client");

                        let data = GitlabData::GroupMembers(ResourceLink {
                            resource_id: group.id.clone(),
                            connection: conn.clone(),
                        });

                        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&data).unwrap();

                        let msg = ClientMessage::PublishRequest {
                            topic: GROUPS_CONSUMER_TOPIC.to_string(),
                            payload: bytes.to_vec(),
                            registration_id: Some(registration_id),
                        };
                        client
                            .send_message(TcpClientMessage::Send(msg))
                            .expect("Expected to send message to tcp client!");
                    });

                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            },
            Err(e) => Err(e.to_string()),
        }
    }

    // helper to get a group memberships given a query operation
    async fn get_group_projects(
        client: Client,
        token: String,
        registration_id: String,
        endpoint: String,
        op: Operation<GroupProjectsQuery, GroupPathVariable>,
    ) -> Result<(), String> {
        debug!("Sending query: {:?}", op.query);

        match client
            .post(endpoint)
            .bearer_auth(token)
            .json(&op)
            .send()
            .await
        {
            Ok(response) => {
                debug!("{response:?}");
                match response.json::<GraphQlResponse<GroupProjectsQuery>>().await {
                    Ok(deserialized) => {
                        if let Some(errors) = deserialized.errors {
                            let errors = errors
                                .iter()
                                .map(|error| error.to_string())
                                .collect::<Vec<_>>()
                                .join("\n");

                            error!("Failed to query instance! {errors}");
                            return Err(errors);
                        }

                        let query = deserialized
                            .data
                            .expect("Expected there to be something here");
                        query.group.map(|group| {
                            let conn = group.projects.unwrap();

                            let client = where_is(BROKER_CLIENT_NAME.to_string())
                                .expect("Expected to find tcp client");

                            let data = GitlabData::GroupProjects(ResourceLink {
                                resource_id: group.id.clone(),
                                connection: conn.clone(),
                            });

                            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&data).unwrap();

                            let msg = ClientMessage::PublishRequest {
                                topic: GROUPS_CONSUMER_TOPIC.to_string(),
                                payload: bytes.to_vec(),
                                registration_id: Some(registration_id),
                            };
                            client
                                .send_message(TcpClientMessage::Send(msg))
                                .expect("Expected to send message to tcp client!");
                        });

                        Ok(())
                    }
                    Err(e) => Err(e.to_string()),
                }
            }
            Err(e) => Err(e.to_string()),
        }
    }

    // helper to get a group memberships given a query operation
    async fn get_group_runners(
        client: Client,
        token: String,
        registration_id: String,
        endpoint: String,
        op: Operation<GroupRunnersQuery, GroupPathVariable>,
    ) -> Result<(), String> {
        debug!("Sending query: {:?}", op.query);

        match client
            .post(endpoint)
            .bearer_auth(token)
            .json(&op)
            .send()
            .await
        {
            Ok(response) => match response.json::<GraphQlResponse<GroupRunnersQuery>>().await {
                Ok(deserialized) => {
                    if let Some(errors) = deserialized.errors {
                        let errors = errors
                            .iter()
                            .map(|error| error.to_string())
                            .collect::<Vec<_>>()
                            .join("\n");

                        error!("Failed to query instance! {errors}");
                        return Err(errors);
                    }

                    let query = deserialized
                        .data
                        .expect("Expected there to be something here");
                    query.group.map(|group| {
                        let conn = group.runners.unwrap();

                        let client = where_is(BROKER_CLIENT_NAME.to_string())
                            .expect("Expected to find tcp client");

                        let data = GitlabData::GroupRunners(ResourceLink {
                            resource_id: group.id.clone(),
                            connection: conn.clone(),
                        });

                        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&data).unwrap();

                        let msg = ClientMessage::PublishRequest {
                            topic: GROUPS_CONSUMER_TOPIC.to_string(),
                            payload: bytes.to_vec(),
                            registration_id: Some(registration_id),
                        };
                        client
                            .send_message(TcpClientMessage::Send(msg))
                            .expect("Expected to send message to tcp client!");
                    });

                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            },
            Err(e) => Err(e.to_string()),
        }
    }
}

#[async_trait]
impl Actor for GitlabGroupObserver {
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
            graphql_endpoint(&args.gitlab_endpoint),
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
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        GitlabGroupObserver::observe(myself, state, state.base_interval);
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
                    Command::GetGroups(op) => {
                        debug!("Sending query: {:?}", op.query);

                        match state
                            .web_client
                            .post(state.gitlab_endpoint.clone())
                            .bearer_auth(state.token.clone().unwrap_or_default())
                            .json(&op)
                            .send()
                            .await
                        {
                            Ok(response) => {
                                debug!("{response:?}");
                                match response.json::<GraphQlResponse<AllGroupsQuery>>().await {
                                    Ok(deserialized) => {
                                        if let Some(errors) = deserialized.errors {
                                            let errors = errors
                                                .iter()
                                                .map(|error| error.to_string())
                                                .collect::<Vec<_>>()
                                                .join("\n");

                                            error!("Failed to query instance! {errors}");
                                            myself
                                                .send_message(GitlabObserverMessage::Backoff(
                                                    BackoffReason::GraphqlError(errors),
                                                ))
                                                .expect(MESSAGE_FORWARDING_FAILED);
                                        } else if let Some(query) = deserialized.data {
                                            if let Some(connection) = query.groups {
                                                let mut read_groups = Vec::new();

                                                if let Some(groups) = connection.nodes {
                                                    for option in groups {
                                                        if let Some(group) = option {
                                                            // find this group's members
                                                            let get_group_members =
                                                                GroupMembersQuery::build(
                                                                    GroupPathVariable {
                                                                        full_path: group
                                                                            .full_path
                                                                            .clone(),
                                                                    },
                                                                );

                                                            GitlabGroupObserver::get_group_members(
                                                                state.web_client.clone(),
                                                                state
                                                                    .token
                                                                    .clone()
                                                                    .unwrap_or_default(),
                                                                state.registration_id.clone(),
                                                                state.gitlab_endpoint.clone(),
                                                                get_group_members,
                                                            )
                                                            .await
                                                            .unwrap();

                                                            let get_group_projects =
                                                                GroupProjectsQuery::build(
                                                                    GroupPathVariable {
                                                                        full_path: group
                                                                            .full_path
                                                                            .clone(),
                                                                    },
                                                                );

                                                            GitlabGroupObserver::get_group_projects(
                                                                state.web_client.clone(),
                                                                state.token.clone().unwrap_or_default(),
                                                                state.registration_id.clone(),
                                                                state.gitlab_endpoint.clone(),
                                                                get_group_projects,
                                                            )
                                                            .await
                                                            .unwrap();

                                                            let get_group_runners =
                                                                GroupRunnersQuery::build(
                                                                    GroupPathVariable {
                                                                        full_path: group
                                                                            .full_path
                                                                            .clone(),
                                                                    },
                                                                );

                                                            GitlabGroupObserver::get_group_runners(
                                                                state.web_client.clone(),
                                                                state
                                                                    .token
                                                                    .clone()
                                                                    .unwrap_or_default(),
                                                                state.registration_id.clone(),
                                                                state.gitlab_endpoint.clone(),
                                                                get_group_runners,
                                                            )
                                                            .await
                                                            .unwrap();

                                                            read_groups.push(group);
                                                        }
                                                    }
                                                }

                                                info!("Observed {0} group(s)", read_groups.len());

                                                let client =
                                                    where_is(BROKER_CLIENT_NAME.to_string())
                                                        .expect("Expected to find tcp client!");

                                                let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(
                                                    &GitlabData::Groups(read_groups),
                                                )
                                                .unwrap();

                                                let msg = ClientMessage::PublishRequest {
                                                    topic: GROUPS_CONSUMER_TOPIC.to_string(),
                                                    payload: bytes.to_vec(),
                                                    registration_id: Some(
                                                        state.registration_id.clone(),
                                                    ),
                                                };
                                                client
                                                    .send_message(TcpClientMessage::Send(msg))
                                                    .expect(
                                                        "Expected to send message to tcp client!",
                                                    );
                                            }
                                        }
                                    }
                                    Err(e) => myself
                                        .send_message(GitlabObserverMessage::Backoff(
                                            BackoffReason::FatalError(e.to_string()),
                                        ))
                                        .expect(MESSAGE_FORWARDING_FAILED),
                                }
                            }
                            Err(e) => myself
                                .send_message(GitlabObserverMessage::Backoff(
                                    BackoffReason::GitlabUnreachable(e.to_string()),
                                ))
                                .expect(MESSAGE_FORWARDING_FAILED),
                        }
                    }
                    _ => warn!(UNEXPECTED_MESSAGE_STR),
                }
            }

            GitlabObserverMessage::Backoff(reason) => {
                // cancel old event loop and start a new one with updated state, if observer hasn't stopped
                if let Some(handle) = &state.task_handle {
                    handle.abort();
                    // start new loop
                    match handle_backoff(state, reason) {
                        Ok(duration) => {
                            GitlabGroupObserver::observe(myself, state, duration);
                        }
                        Err(e) => myself.stop(Some(e.to_string())),
                    }
                }
            }
        }
        Ok(())
    }
}
