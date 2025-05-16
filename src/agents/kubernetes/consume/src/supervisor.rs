use std::time::Duration;

use cassini::client::*;
use kube_common::MessageDispatcher;
use polar::{DISPATCH_ACTOR, get_neo_config};
use ractor::async_trait;
use ractor::registry::where_is;
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

use crate::pods::PodConsumer;
// use crate::pods::PodConsumer;
use crate::KubeConsumerArgs;
use crate::BROKER_CLIENT_NAME;

pub struct ClusterConsumerSupervisor;

pub struct ClusterConsumerSupervisorState {
    max_registration_attempts: u32, // amount of times the supervisor will try to get the session_id from the client
    graph_config: neo4rs::Config,
}

pub enum ClusterConsumerSupervisorMessage {
    TopicMessage { topic: String, payload: String },
}

pub struct ClusterConsumerSupervisorArgs {
    pub client_config: cassini::TCPClientConfig,
    pub graph_config: neo4rs::Config
}


#[async_trait]
impl Actor for ClusterConsumerSupervisor {
    type Msg = ClusterConsumerSupervisorMessage;
    type State = ClusterConsumerSupervisorState;
    type Arguments = ClusterConsumerSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ClusterConsumerSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let state = ClusterConsumerSupervisorState {
            max_registration_attempts: 5,
            graph_config: get_neo_config()
        };

        let _ = Actor::spawn_linked(
            Some(DISPATCH_ACTOR.to_string()),
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
                //when client starts successfully, start workers

                // Set up an interval
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
                        // if we got got registered, start actors.
                        // At present, the only reason consumer should fail on startup is if they're in an insane state.
                        // In this case, failing to find the GRAPH_CA_CERT file. In which case, there's nothing we can really do.
                        // Should that happen, we should just log the error and stop
                        if let Some(registration_id) = result {

                            //start actors
                            let args = KubeConsumerArgs {
                                registration_id, graph_config: state.graph_config.clone() 
                            };
                                
                            // TODO: The anticipated naming convention for these will be kubernetes.clustername.rrole.resource - but cluster name isn't really known ahead of time at the moment.
                            // For now, we'll test using either minikube or kind, so for now we can simply use kubernetes.cluster.default.pods
                            if let Err(e) = Actor::spawn_linked(Some("kubernetes.cluster.consumer.PodList".to_string()), PodConsumer, args, myself.get_cell().clone()).await {
                                error!("{e}");
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
        _: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        state: &mut Self::State,
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
                // we no actors start w/o names
                let actor_name = actor_cell.get_name().unwrap();

                info!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_name,
                    actor_cell.get_id()
                );

            //    match ClusterConsumerSupervisor::restart_actor(actor_name.clone(), myself.clone(), state.graph_config.clone()).await {
            //         Ok(actor) => {
            //             info!("Restarted actor {0:?}:{1:?}", 
            //             actor_name,
            //             actor.get_id())
            //         }
            //         Err(e) => {
            //             error!("Failed to recover actor: {e}");
            //             myself.stop(Some(e))
            //         }
            //    }
                                    
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                // we no actors start w/o names
                let actor_name = actor_cell.get_name().unwrap();

                warn!(
                    "Consumer_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_name,
                    actor_cell.get_id()
                );
            //     match ClusterConsumerSupervisor::restart_actor(actor_name.clone(), myself.clone(), state.graph_config.clone()).await {
            //         Ok(actor) => {
            //             info!("Restarted actor {0:?}:{1:?}", 
            //             actor_name,
            //             actor.get_id())
            //         }
            //         Err(e) => {
            //             error!("Failed to recover actor: {e}");
            //             myself.stop(Some(e))
            //         }
            //    }
            }
    
            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }

        Ok(())
    }
}
