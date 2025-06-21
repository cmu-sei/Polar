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

use cassini::{client::TcpClientMessage, ClientMessage};
use common::{types::GitlabData, RUNNERS_CONSUMER_TOPIC};
use cynic::{GraphQlResponse, QueryBuilder};
use gitlab_queries::runners::*;

use crate::{
    handle_backoff, BackoffReason, Command, GitlabObserverArgs, GitlabObserverMessage,
    GitlabObserverState,
};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, error, info, warn};

use crate::BROKER_CLIENT_NAME;

pub struct GitlabRunnerObserver;

impl GitlabRunnerObserver {
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
                //build query
                let op = MultiRunnerQuery::build(MultiRunnerQueryArguments {
                    paused: Some(false),
                    status: None,
                    tag_list: None,
                    search: None,
                    creator_id: None,
                });

                // pass query in message
                let command = Command::GetRunners(op);
                GitlabObserverMessage::Tick(command)
            })
            .abort_handle();

        state.task_handle = Some(handle);
    }
}
#[async_trait]
impl Actor for GitlabRunnerObserver {
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
            Duration::from_secs(args.max_backoff),
        );
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("{myself:?} starting...");

        GitlabRunnerObserver::observe(myself, state, state.base_interval);

        Ok(())
    }
    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            GitlabObserverMessage::Tick(command) => match command {
                Command::GetRunners(op) => {
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
                            match response.json::<GraphQlResponse<MultiRunnerQuery>>().await {
                                Ok(deserialized) => {
                                    if let Some(errors) = deserialized.errors {
                                        let errors = errors
                                            .iter()
                                            .map(|error| error.to_string())
                                            .collect::<Vec<_>>()
                                            .join("\n");

                                        if let Err(e) =
                                            myself.send_message(GitlabObserverMessage::Backoff(
                                                BackoffReason::GraphqlError(errors),
                                            ))
                                        {
                                            error!("{e}");
                                            myself.stop(Some(e.to_string()))
                                        }
                                    } else if let Some(query) = deserialized.data {
                                        if let Some(connection) = query.runners {
                                            let mut read_runners = Vec::new();

                                            if let Some(runners) = connection.nodes {
                                                for option in runners {
                                                    if let Some(runner) = option {
                                                        read_runners.push(runner);
                                                    }
                                                }
                                            }

                                            info!("Observed {0} runner(s)", read_runners.len());

                                            let client = where_is(BROKER_CLIENT_NAME.to_string())
                                                .expect("Expected to find tcp client!");

                                            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(
                                                &GitlabData::Runners(read_runners),
                                            )
                                            .unwrap();

                                            let msg = ClientMessage::PublishRequest {
                                                topic: RUNNERS_CONSUMER_TOPIC.to_string(),
                                                payload: bytes.to_vec(),
                                                registration_id: Some(
                                                    state.registration_id.clone(),
                                                ),
                                            };
                                            client
                                                .send_message(TcpClientMessage::Send(msg))
                                                .expect("Expected to send message to tcp client!");
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("Failed to deserialize query {e}");
                                    if let Err(e) =
                                        myself.send_message(GitlabObserverMessage::Backoff(
                                            BackoffReason::FatalError(e.to_string()),
                                        ))
                                    {
                                        error!("{e}");
                                        myself.stop(Some(e.to_string()))
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Error observing data: {e}");
                            if let Err(e) = myself.send_message(GitlabObserverMessage::Backoff(
                                BackoffReason::GitlabUnreachable(e.to_string()),
                            )) {
                                error!("{e}");
                                myself.stop(Some(e.to_string()))
                            }
                        }
                    }
                }
                _ => (),
            },
            GitlabObserverMessage::Backoff(reason) => {
                // cancel old event loop and start a new one with updated state, if observer hasn't stopped
                if let Some(handle) = &state.task_handle {
                    handle.abort();
                    // start new loop
                    match handle_backoff(state, reason) {
                        Ok(duration) => {
                            GitlabRunnerObserver::observe(myself, state, duration);
                        }
                        Err(e) => myself.stop(Some(e.to_string())),
                    }
                }
            }
        }

        Ok(())
    }
}
