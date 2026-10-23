/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

use crate::{
    BackoffReason, Command, GitlabObserverMessage, graphql_endpoint, handle_backoff,
    handle_graphql_errors, init_observer_state, send_to_broker,
};
use crate::{GitlabObserverArgs, GitlabObserverState, MESSAGE_FORWARDING_FAILED};
use common::METADATA_CONSUMER_TOPIC;
use common::types::{GitlabData, GitlabInstance};
use cynic::GraphQlResponse;
use cynic::QueryBuilder;
use gitlab_queries::{LicenseHistoryEntry, LicenseHistoryQuery, MetadataQuery};
use ractor::{Actor, ActorProcessingErr, ActorRef, concurrency::Duration};

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
                        } else if let Some(query_result) = deserialized.data
                            && let Some(metadata) = query_result.metadata
                        {
                            //package and send

                            //TODO: Get base url w/o api
                            let instance = GitlabInstance {
                                base_url: state.gitlab_endpoint.clone(),
                                metadata,
                            };

                            let data = GitlabData::Instance(instance);

                            send_to_broker(state, data, METADATA_CONSUMER_TOPIC)?;
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
                            Ok(())
                        } else if let Some(query_result) = deserialized.data {
                            query_result.license_history_entries.map(|connection| {
                                //package and send
                                let mut read_licenses: Vec<LicenseHistoryEntry> = Vec::new();

                                if let Some(licenses) = connection.nodes {
                                    read_licenses.extend(licenses.into_iter().flatten());
                                }

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
            GitlabObserverMessage::Tick(command) => {
                if matches!(command, Command::GetMetadata) {
                    MetaObserver::get_metadata(state, myself).await.unwrap();
                }
                // other commands are ignored
            }
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
