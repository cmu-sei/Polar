use std::time::Duration;

use cassini::client::*;
use common::dispatch::MessageDispatcher;
use common::GROUPS_CONSUMER_TOPIC;
use common::PROJECTS_CONSUMER_TOPIC;
use common::RUNNERS_CONSUMER_TOPIC;
use common::USER_CONSUMER_TOPIC;
use ractor::async_trait;
use ractor::rpc::call;
use ractor::rpc::CallResult;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::groups::GitlabGroupConsumer;
use crate::projects::GitlabProjectConsumer;
use crate::runners::GitlabRunnerConsumer;
use crate::users::GitlabUserConsumer;
use crate::GitlabConsumerArgs;
use crate::BROKER_CLIENT_NAME;
use crate::GITLAB_USER_CONSUMER;

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    max_registration_attempts: u32, // amount of times the supervisor will try to get the session_id from the client
}

pub enum ConsumerSupervisorMessage {
    TopicMessage { topic: String, payload: String },
}

pub struct ConsumerSupervisorArgs {
    pub client_config: cassini::TCPClientConfig
}

#[async_trait]
impl Actor for ConsumerSupervisor {
    type Msg = ConsumerSupervisorMessage;
    type State = ConsumerSupervisorState;
    type Arguments = ConsumerSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ConsumerSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let state = ConsumerSupervisorState {
            max_registration_attempts: 5,
        };
        // start dispatcher
        let _ = Actor::spawn_linked(
            Some("DISPATCH".to_string()),
            MessageDispatcher,
            (),
            myself.clone().into(),
        )
        .await
        .expect("Expected to start dispatcher");
        match Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config: args.client_config,
                registration_id: None,
            },
            myself.clone().into(),
        )
        .await
        {
            Ok((client, _)) => {
                //when client starts, successfully, start workers
                // Set up an interval
                //TODO: make configurable
                let mut interval = tokio::time::interval(Duration::from_secs(5));
                //wait until we get a session id to start clients, try some configured amount of times every few seconds
                let mut attempts = 0;
                loop {
                    attempts += 1;
                    info!("Getting session data...");
                    if let CallResult::Success(result) = call(
                        &client,
                        |reply| TcpClientMessage::GetRegistrationId(reply),
                        None,
                    )
                    .await
                    .expect("Expected to call client!")
                    {
                        if let Some(registration_id) = result {
                            let args = GitlabConsumerArgs { registration_id };

                            if let Err(e) = Actor::spawn_linked(
                                Some(USER_CONSUMER_TOPIC.to_string()),
                                GitlabUserConsumer,
                                args.clone(),
                                myself.clone().into(),
                            )
                            .await
                            {
                                warn!("failed to start users consumer {e}")
                            }
                            if let Err(e) = Actor::spawn_linked(
                                Some(GROUPS_CONSUMER_TOPIC.to_string()),
                                GitlabGroupConsumer,
                                args.clone(),
                                myself.clone().into(),
                            )
                            .await
                            {
                                warn!("failed to start groups consumer {e}")
                            }
                            if let Err(e) = Actor::spawn_linked(
                                Some(RUNNERS_CONSUMER_TOPIC.to_string()),
                                GitlabRunnerConsumer,
                                args.clone(),
                                myself.clone().into(),
                            )
                            .await
                            {
                                warn!("failed to start runners consumer {e}")
                            }
                            if let Err(e) = Actor::spawn_linked(
                                Some(PROJECTS_CONSUMER_TOPIC.to_string()),
                                GitlabProjectConsumer,
                                args.clone(),
                                myself.clone().into(),
                            )
                            .await
                            {
                                warn!("failed to start projects consumer {e}")
                            }

                            break;
                        } else if attempts < state.max_registration_attempts {
                            warn!("Failed to get session data. Retrying.");
                        } else if attempts >= state.max_registration_attempts {
                            error!("Failed to retrieve session data! timed out");
                            myself.stop(Some(
                                "Failed to retrieve session data! timed out".to_string(),
                            ));
                        }
                    }
                    interval.tick().await;
                }
            }
            Err(e) => {
                error!("{e}");
                myself.stop(None);
            }
        }

        Ok(state)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            _ => todo!(),
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(actor_cell) => {
                info!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} started",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                info!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!(
                    "Consumer_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }

        Ok(())
    }
}
