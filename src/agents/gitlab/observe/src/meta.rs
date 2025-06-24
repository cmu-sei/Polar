use crate::{
    graphql_endpoint, handle_backoff, handle_graphql_errors, BackoffReason, Command,
    GitlabObserverMessage, BROKER_CLIENT_NAME,
};
use crate::{GitlabObserverArgs, GitlabObserverState};
use cassini::client::TcpClientMessage;
use cassini::ClientMessage;
use common::types::{GitlabData, GitlabInstance};
use common::METADATA_CONSUMER_TOPIC;
use cynic::GraphQlResponse;
use cynic::QueryBuilder;
use gitlab_queries::{LicenseHistoryEntry, LicenseHistoryQuery, MetadataQuery};
use ractor::registry::where_is;
use ractor::{concurrency::Duration, Actor, ActorProcessingErr, ActorRef};
use rkyv::rancor;

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
    ) {
        let operation = MetadataQuery::build(());

        //execute
        debug!("Sending query: {}", operation.query);

        match state
            .web_client
            .post(state.gitlab_endpoint.clone())
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

                                let message = GitlabData::Instance(instance);

                                if let Ok(seriarlized) = rkyv::to_bytes::<rancor::Error>(&message) {
                                    let client = where_is(BROKER_CLIENT_NAME.to_string()).unwrap();

                                    let msg = ClientMessage::PublishRequest {
                                        topic: METADATA_CONSUMER_TOPIC.to_string(),
                                        payload: seriarlized.to_vec(),
                                        registration_id: Some(state.registration_id.clone()),
                                    };

                                    client
                                        .send_message(TcpClientMessage::Send(msg))
                                        .expect("Expected to send message");
                                }
                            }
                        }
                    }
                    Err(e) => todo!(),
                }
            }
            Err(e) => {
                error!("{e}");
                actor_ref
                    .send_message(GitlabObserverMessage::Backoff(
                        BackoffReason::GitlabUnreachable(e.to_string()),
                    ))
                    .expect("Expected to forward message to self")
            }
        }

        // --- get licenses ---
        let operation = LicenseHistoryQuery::build(());

        //execute
        debug!("Sending query: {}", operation.query);

        match state
            .web_client
            .post(state.gitlab_endpoint.clone())
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
                        } else if let Some(query_result) = deserialized.data {
                            if let Some(connection) = query_result.license_history_entries {
                                //package and send
                                let mut read_licenses: Vec<LicenseHistoryEntry> = Vec::new();

                                if let Some(licenses) = connection.nodes {
                                    read_licenses.extend(licenses.into_iter().filter_map(
                                        |option| {
                                            option.map(|license| {
                                                //extract license information during iteration
                                                license
                                            })
                                        },
                                    ));
                                }

                                let message = GitlabData::Licenses(read_licenses);

                                if let Ok(seriarlized) = rkyv::to_bytes::<rancor::Error>(&message) {
                                    let client = where_is(BROKER_CLIENT_NAME.to_string()).unwrap();

                                    let msg = ClientMessage::PublishRequest {
                                        topic: METADATA_CONSUMER_TOPIC.to_string(),
                                        payload: seriarlized.to_vec(),
                                        registration_id: Some(state.registration_id.clone()),
                                    };

                                    client
                                        .send_message(TcpClientMessage::Send(msg))
                                        .expect("Expected to send message");
                                }
                            }
                        }
                    }
                    Err(_e) => todo!("handle deserialization error"),
                }
            }
            Err(e) => {
                error!("{e}");
                actor_ref
                    .send_message(GitlabObserverMessage::Backoff(
                        BackoffReason::GitlabUnreachable(e.to_string()),
                    ))
                    .expect("Expected to forward message to self")
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
