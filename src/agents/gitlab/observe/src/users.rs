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
use std::time::Duration;

use crate::{
    handle_backoff, init_observer_state, send_to_broker, BackoffReason, Command,
    GitlabObserverArgs, GitlabObserverMessage, GitlabObserverState, BROKER_CLIENT_NAME,
    MESSAGE_FORWARDING_FAILED,
};
use common::USER_CONSUMER_TOPIC;
use cynic::GraphQlResponse;

use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};

use common::types::{GitlabData, ResourceLink};
use cynic::QueryBuilder;
use gitlab_queries::users::*;

use tracing::{debug, error, info, warn};

pub struct GitlabUserObserver;

impl GitlabUserObserver {
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
                let op = MultiUserQuery::build(MultiUserQueryArguments {
                    after: None,
                    admins: None,
                    active: Some(true),
                    ids: None,
                    usernames: None,
                    humans: None,
                });

                let command = Command::GetUsers(op);
                //execute query
                GitlabObserverMessage::Tick(command)
            })
            .abort_handle();

        state.task_handle = Some(handle);
    }
}
#[async_trait]
impl Actor for GitlabUserObserver {
    type Msg = GitlabObserverMessage;
    type State = GitlabObserverState;
    type Arguments = GitlabObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabObserverArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let state = init_observer_state(args);

        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // start initial event loop, and manage the handle in state.
        GitlabUserObserver::observe(myself, state, state.base_interval);
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
                    Command::GetUsers(op) => {
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
                                //forwrard to client
                                match response.json::<GraphQlResponse<MultiUserQuery>>().await {
                                    Ok(deserialized) => {
                                        if let Some(errors) = deserialized.errors {
                                            let errors = errors
                                                .iter()
                                                .map(|error| error.to_string())
                                                .collect::<Vec<_>>()
                                                .join("\n");

                                            error!("Failed to query instance! {errors}");

                                            if let Err(e) =
                                                myself.send_message(GitlabObserverMessage::Backoff(
                                                    BackoffReason::GraphqlError(errors),
                                                ))
                                            {
                                                error!("{e}");
                                                myself.stop(Some(e.to_string()))
                                            }
                                        } else if let Some(query) = deserialized.data {
                                            if let Some(connection) = query.users {
                                                info!("Found {} user(s)", connection.count);

                                                let mut read_users: Vec<UserCoreFragment> =
                                                    Vec::new();

                                                if let Some(users) = connection.nodes {
                                                    // Append nodes to the result list.
                                                    read_users.extend(users.into_iter().filter_map(
                                                    |option| {
                                                        option.map(|user| {
                                                            //extract user information during iteration
                                                            user.project_memberships.as_ref().map(
                                                                |connection| {

                                                                    let data
                                                                        = GitlabData::ProjectMembers(
                                                                            ResourceLink {
                                                                                resource_id: user
                                                                                    .id
                                                                                    .clone(),
                                                                                connection: connection
                                                                                    .clone(),
                                                                            },
                                                                        );

                                                                    send_to_broker(state, data, USER_CONSUMER_TOPIC).map_err(|e| error!("{e}"))
                                                                },
                                                            );

                                                            user
                                                        })
                                                    },
                                                ));
                                                }

                                                let data = GitlabData::Users(read_users);

                                                if let Err(e) =
                                                    send_to_broker(state, data, USER_CONSUMER_TOPIC)
                                                {
                                                    return Err(e);
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        error!("Failed to deserialize response from server! {e}");
                                        myself
                                            .send_message(GitlabObserverMessage::Backoff(
                                                BackoffReason::FatalError(e.to_string()),
                                            ))
                                            .expect(MESSAGE_FORWARDING_FAILED);
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Error observing data: {e}");
                                myself
                                    .send_message(GitlabObserverMessage::Backoff(
                                        BackoffReason::GitlabUnreachable(e.to_string()),
                                    ))
                                    .expect(MESSAGE_FORWARDING_FAILED);
                            }
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
                            GitlabUserObserver::observe(myself, state, duration);
                        }
                        Err(e) => myself.stop(Some(e.to_string())),
                    }
                }
            }
        }

        Ok(())
    }
}
