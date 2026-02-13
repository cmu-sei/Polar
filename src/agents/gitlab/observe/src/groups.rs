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
use serde_json::from_str;

use crate::{
    graphql_endpoint, handle_backoff, init_observer_state, send_to_broker, BackoffReason, Command,
    GitlabObserverArgs, GitlabObserverMessage, GitlabObserverState, MESSAGE_FORWARDING_FAILED,
};
use common::{
    types::{GitlabData, ResourceLink},
    GROUPS_CONSUMER_TOPIC,
};
use cynic::{GraphQlResponse, Operation, QueryBuilder};
use gitlab_queries::groups::{
    AllGroupsQuery, GroupMembersQuery, GroupPathVariable, GroupProjectsQuery, GroupRunnersQuery,
    MultiGroupQueryArguments,
};
use polar::UNEXPECTED_MESSAGE_STR;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};

use tracing::{debug, error, info, warn};

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
        state: &mut GitlabObserverState,
        op: Operation<GroupMembersQuery, GroupPathVariable>,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Sending query: {:?}", op.query);

        let token = state.token.clone().unwrap_or_default();

        match state
            .web_client
            .post(graphql_endpoint(&state.gitlab_endpoint))
            .bearer_auth(token)
            .json(&op)
            .send()
            .await
        {
            Ok(response) => {
                // extract text
                match response.text().await {
                    Ok(content) => {
                        debug!("Received response: {}", content);

                        match from_str::<GraphQlResponse<GroupMembersQuery>>(&content) {
                            Ok(deserialized) => {
                                if let Some(errors) = deserialized.errors {
                                    let errors = errors
                                        .iter()
                                        .map(|error| error.to_string())
                                        .collect::<Vec<_>>()
                                        .join("\n");

                                    error!("Failed to query instance! {errors}");
                                    return Err(errors.into());
                                }

                                let query = deserialized
                                    .data
                                    .expect("Expected there to be something here");
                                query.group.map(|group| {
                                    let conn = group.group_members.unwrap();

                                    let data = GitlabData::GroupMembers(ResourceLink {
                                        resource_id: group.id.clone(),
                                        connection: conn.clone(),
                                    });

                                    send_to_broker(state, data, GROUPS_CONSUMER_TOPIC)
                                        .map_err(|e| error!("{e}"))
                                });

                                Ok(())
                            }
                            Err(e) => {
                                error!("{e}");
                                Err(e.into())
                            }
                        }
                    }
                    Err(e) => Err(e.into()),
                }
            }
            Err(e) => Err(e.into()),
        }
    }

    // helper to get a group memberships given a query operation
    async fn get_group_projects(
        state: &mut GitlabObserverState,
        op: Operation<GroupProjectsQuery, GroupPathVariable>,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Sending query: {:?}", op.query);

        let token = state.token.clone().unwrap_or_default();

        match state
            .web_client
            .post(graphql_endpoint(&state.gitlab_endpoint))
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
                            return Err(errors.into());
                        }

                        let query = deserialized
                            .data
                            .expect("Expected there to be something here");
                        query.group.map(|group| {
                            let conn = group.projects.unwrap();

                            let data = GitlabData::GroupProjects(ResourceLink {
                                resource_id: group.id.clone(),
                                connection: conn.clone(),
                            });

                            send_to_broker(state, data, GROUPS_CONSUMER_TOPIC)
                                .map_err(|e| error!("{e}"))
                        });

                        Ok(())
                    }
                    Err(e) => Err(e.into()),
                }
            }
            Err(e) => Err(e.into()),
        }
    }

    // helper to get a group memberships given a query operation
    async fn get_group_runners(
        state: &mut GitlabObserverState,
        op: Operation<GroupRunnersQuery, GroupPathVariable>,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Sending query: {:?}", op.query);

        let token = state.token.clone().unwrap_or_default();

        match state
            .web_client
            .post(graphql_endpoint(&state.gitlab_endpoint))
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
                        return Err(errors.into());
                    }

                    let query = deserialized
                        .data
                        .expect("Expected there to be something here");
                    query.group.map(|group| {
                        let conn = group.runners.unwrap();

                        let data = GitlabData::GroupRunners(ResourceLink {
                            resource_id: group.id.clone(),
                            connection: conn.clone(),
                        });

                        return send_to_broker(state, data, GROUPS_CONSUMER_TOPIC);
                    });

                    Ok(())
                }
                Err(e) => Err(e.into()),
            },
            Err(e) => Err(e.into()),
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

        Ok(init_observer_state(args))
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
                            .post(graphql_endpoint(&state.gitlab_endpoint))
                            .bearer_auth(state.token.clone().unwrap_or_default())
                            .json(&op)
                            .send()
                            .await
                        {
                            Ok(response) => {
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

                                                            if let Err(e) = GitlabGroupObserver::get_group_members(
                                                                state,
                                                                get_group_members,
                                                            )
                                                            .await
                                                            {
                                                                error!("Failed to get group members {e}");
                                                                return Err(ActorProcessingErr::from(e))
                                                            }

                                                            let get_group_projects =
                                                                GroupProjectsQuery::build(
                                                                    GroupPathVariable {
                                                                        full_path: group
                                                                            .full_path
                                                                            .clone(),
                                                                    },
                                                                );

                                                            if let Err(e) = GitlabGroupObserver::get_group_projects(
                                                                state,
                                                                get_group_projects,
                                                            )
                                                            .await
                                                            {
                                                                error!("Failed to get group projects {e}");
                                                                return Err(ActorProcessingErr::from(e));
                                                            }

                                                            let get_group_runners =
                                                                GroupRunnersQuery::build(
                                                                    GroupPathVariable {
                                                                        full_path: group
                                                                            .full_path
                                                                            .clone(),
                                                                    },
                                                                );

                                                            if let Err(e) =GitlabGroupObserver::get_group_runners(
                                                                state,
                                                                get_group_runners,
                                                            )
                                                            .await {
                                                                error!("Failed to get group runners {e}");
                                                                return Err(ActorProcessingErr::from(e));
                                                            }

                                                            read_groups.push(group);
                                                        }
                                                    }
                                                }

                                                info!("Observed {0} group(s)", read_groups.len());

                                                let data = GitlabData::Groups(read_groups);

                                                if let Err(e) = send_to_broker(
                                                    state,
                                                    data,
                                                    GROUPS_CONSUMER_TOPIC,
                                                ) {
                                                    return Err(e);
                                                }
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
