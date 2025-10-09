use cassini_client::*;
use kube_common::MessageDispatcher;
use polar::{get_neo_config, DISPATCH_ACTOR};
use ractor::async_trait;
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
    graph_config: neo4rs::Config,
}

pub enum SupervisorMessage {
    ClientRegistered(String),
}

pub struct ClusterConsumerSupervisorArgs {
    pub client_config: cassini::TCPClientConfig,
    pub graph_config: neo4rs::Config,
}

#[async_trait]
impl Actor for ClusterConsumerSupervisor {
    type Msg = SupervisorMessage;
    type State = ClusterConsumerSupervisorState;
    type Arguments = ClusterConsumerSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ClusterConsumerSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let output_port = std::sync::Arc::new(ractor::OutputPort::default());

        output_port.subscribe(myself.clone(), |message| {
            Some(SupervisorMessage::ClientRegistered(message))
        });

        match Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config: args.client_config,
                registration_id: None,
                output_port,
            },
            myself.clone().into(),
        )
        .await
        {
            Ok(_) => {
                let state = ClusterConsumerSupervisorState {
                    graph_config: get_neo_config(),
                };

                let _ = Actor::spawn_linked(
                    Some(DISPATCH_ACTOR.to_string()),
                    MessageDispatcher,
                    (),
                    myself.clone().into(),
                )
                .await
                .expect("Expected to start dispatcher");

                Ok(state)
            }
            Err(e) => {
                error!("{e}");
                Err(ActorProcessingErr::from(e))
            }
        }
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::ClientRegistered(registration_id) => {
                //start actors
                let args = KubeConsumerArgs {
                    registration_id,
                    graph_config: state.graph_config.clone(),
                };

                // TODO: The anticipated naming convention for these will be kubernetes.clustername.rrole.resource - but cluster name isn't really known ahead of time at the moment.
                // For now, we'll test using either minikube or kind, so for now we can simply use kubernetes.cluster.default.pods
                if let Err(e) = Actor::spawn_linked(
                    Some(kube_common::get_consumer_name("cluster", "Pod")),
                    PodConsumer,
                    args,
                    myself.get_cell().clone(),
                )
                .await
                {
                    error!("{e}");
                }
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _state: &mut Self::State,
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
