use crate::{
    graphql_endpoint, handle_backoff, handle_graphql_errors, init_observer_state, send_to_broker,
    BackoffReason, Command, GitlabObserverMessage,
};
use crate::{GitlabObserverArgs, GitlabObserverState, MESSAGE_FORWARDING_FAILED};
use common::types::{GitlabData, GitlabInstance};
use common::METADATA_CONSUMER_TOPIC;
use cynic::GraphQlResponse;
use cynic::QueryBuilder;
use gitlab_queries::{LicenseHistoryEntry, LicenseHistoryQuery, MetadataQuery};
use ractor::{concurrency::Duration, Actor, ActorProcessingErr, ActorRef};

use tracing::{debug, error, info};

pub struct MetaObserver;

impl MetaObserver {
    fn observe(
        myself: ActorRef<GitlabObserverMessage>,
        state: &mut GitlabObserverState,
        duration: Duration,
    ) {
        info!("Observing every {} seconds", duration.as_secs());
        let handle = myself
            .send_interval(duration, || {
                GitlabObserverMessage::Tick(Command::GetMetadata)
            })
            .abort_handle();

        state.task_handle = Some(handle);
    }

    async fn get_metadata(
        state: &mut GitlabObserverState,
        actor_ref: ActorRef<GitlabObserverMessage>,
    ) -> Result<(), ActorProcessingErr> {
        let operation = MetadataQuery::build(());

        //execute
        debug!("Sending query: {}", operation.query);

        match state
            .web_client
            .post(graphql_endpoint(&state.gitlab_endpoint))
            .bearer_auth(state.token.clone().unwrap_or_default())
            .json(&operation)
            .send()
            .await
        {
            Ok(response) => {
                //forwrard to client
                match response.json::<GraphQlResponse<MetadataQuery>>().await {
                    Ok(deserialized) => {
                        if let Some(errors) = deserialized.errors {
                            handle_graphql_errors(errors, actor_ref.clone());
                        } else if let Some(query_result) = deserialized.data {
                            if let Some(metadata) = query_result.metadata {
                                //package and send

                                //TODO: Get base url w/o api
                                let instance = GitlabInstance {
                                    base_url: state.gitlab_endpoint.clone(),
                                    metadata,
                                };

                                let data = GitlabData::Instance(instance);

                                if let Err(e) = send_to_broker(state, data, METADATA_CONSUMER_TOPIC)
                                {
                                    return Err(e);
                                }
                            }
                        }
                    }
                    Err(e) => actor_ref
                        .send_message(GitlabObserverMessage::Backoff(
                            BackoffReason::GitlabUnreachable(e.to_string()),
                        ))
                        .expect(MESSAGE_FORWARDING_FAILED),
                }
            }
            Err(e) => {
                error!("{e}");
                actor_ref
                    .send_message(GitlabObserverMessage::Backoff(
                        BackoffReason::GitlabUnreachable(e.to_string()),
                    ))
                    .expect(MESSAGE_FORWARDING_FAILED)
            }
        }

        // --- get licenses ---
        let operation = LicenseHistoryQuery::build(());

        //execute
        debug!("Sending query: {}", operation.query);

        match state
            .web_client
            .post(graphql_endpoint(&state.gitlab_endpoint))
            .bearer_auth(state.token.clone().unwrap_or_default())
            .json(&operation)
            .send()
            .await
        {
            Ok(response) => {
                //forwrard to client
                match response
                    .json::<GraphQlResponse<LicenseHistoryQuery>>()
                    .await
                {
                    Ok(deserialized) => {
                        if let Some(errors) = deserialized.errors {
                            handle_graphql_errors(errors, actor_ref.clone());
                            return Ok(());
                        } else if let Some(query_result) = deserialized.data {
                            query_result.license_history_entries.map(|connection| {
                                //package and send
                                let mut read_licenses: Vec<LicenseHistoryEntry> = Vec::new();

                                connection.nodes.map(|licenses| {
                                    read_licenses.extend(licenses.into_iter().filter_map(
                                        |option| {
                                            option.map(|license| {
                                                //extract license information during iteration
                                                license
                                            })
                                        },
                                    ));
                                });

                                let data = GitlabData::Licenses(read_licenses);

                                send_to_broker(state, data, METADATA_CONSUMER_TOPIC)
                            });
                            Ok(())
                        } else {
                            Ok(())
                        }
                    }
                    Err(e) => {
                        error!("{e}");
                        actor_ref
                            .send_message(GitlabObserverMessage::Backoff(
                                BackoffReason::FatalError(e.to_string()),
                            ))
                            .expect(MESSAGE_FORWARDING_FAILED);
                        Ok(())
                    }
                }
            }
            Err(e) => {
                error!("{e}");
                actor_ref
                    .send_message(GitlabObserverMessage::Backoff(
                        BackoffReason::GitlabUnreachable(e.to_string()),
                    ))
                    .expect(MESSAGE_FORWARDING_FAILED);
                Ok(())
            }
        }
    }

    // fn get_licenses(state: &mut GitlabObserverState, actor_ref: ActorRef<GitlabObserverMessage>) {
    //     todo!()
    // }
}
#[ractor::async_trait]
impl Actor for MetaObserver {
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
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // start initial event loop, and manage the handle in state.
        MetaObserver::observe(myself, state, state.base_interval);
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
                Command::GetMetadata => {
                    MetaObserver::get_metadata(state, myself).await;
                }
                _ => (), // ignore other messages
            },
            GitlabObserverMessage::Backoff(reason) => {
                // cancel old event loop and start a new one with updated state, if observer hasn't stopped
                if let Some(handle) = &state.task_handle {
                    handle.abort();
                    // start new loop
                    match handle_backoff(state, reason) {
                        Ok(duration) => {
                            MetaObserver::observe(myself, state, duration);
                        }
                        Err(e) => myself.stop(Some(e.to_string())),
                    }
                }
            }
        }
        Ok(())
    }
}
