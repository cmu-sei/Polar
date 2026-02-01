use cassini_client::*;
use cassini_types::ClientEvent;
use kube_common::{KubeMessage, RawKubeEvent};
use neo4rs::Graph;
use polar::get_neo_config;
use polar::{Supervisor, SupervisorMessage};
use ractor::async_trait;
use ractor::registry::where_is;
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
use crate::BROKER_CLIENT_NAME;
use crate::{ClusterGraphController, KubeConsumerArgs};
use kube_common::{
    get_consumer_name, BATCH_PROCESS_ACTION, RESOURCE_APPLIED_ACTION, RESOURCE_DELETED_ACTION,
    RESYNC_ACTIUON,
};
pub struct ClusterConsumerSupervisor;

pub struct ClusterConsumerSupervisorState {
    graph_config: neo4rs::Config,
    broker_client: ActorRef<TcpClientMessage>,
}

impl Supervisor for ClusterConsumerSupervisor {
    fn deserialize_and_dispatch(_topic: String, payload: Vec<u8>) {
        // 1) Parse the raw message into your RawKubeEvent
        let raw_event: RawKubeEvent = match serde_json::from_slice(&payload) {
            Ok(ev) => ev,
            Err(err) => {
                error!("Failed to deserialize RawKubeEvent: {}", err);
                return;
            }
        };

        let kind = raw_event.kind; // e.g. "Pod"
        let action = raw_event.action; // e.g. "Applied" or "BatchProcess"
        let payload = raw_event.object; // serde_json::Value

        // 2) Build the consumer’s name from cluster + kind
        let consumer_name = get_consumer_name("cluster", &kind);

        // 3) Look up the actor in Ractor’s registry
        match where_is(consumer_name.clone()) {
            Some(consumer_ref) => {
                // 4) Build a typed KubeMessage based on action
                let kube_msg = match action.as_str() {
                    BATCH_PROCESS_ACTION => KubeMessage::ResourceBatch {
                        kind: kind.clone(),
                        resources: payload,
                    },
                    RESOURCE_APPLIED_ACTION => KubeMessage::ResourceApplied {
                        kind: kind.clone(),
                        resource: payload,
                    },
                    RESOURCE_DELETED_ACTION => KubeMessage::ResourceDeleted {
                        kind: kind.clone(),
                        resource: payload,
                    },
                    RESYNC_ACTIUON => KubeMessage::ResyncStarted {
                        kind: kind.clone(),
                        resource: payload,
                    },
                    other => {
                        warn!(
                            "Unhandled K8s action \"{}\" for kind \"{}\", dropping",
                            other, kind
                        );
                        return;
                    }
                };

                // 5) Send message, logging any failure
                if let Err(err) = consumer_ref.send_message(kube_msg) {
                    error!(
                        "Failed to send KubeMessage to {}: {}",
                        consumer_ref.get_name().unwrap_or_default(),
                        err
                    );
                }
            }

            None => {
                // No consumer registered for this kind
                warn!(
                    "No consumer found for topic \"{}\" (kind=\"{}\")",
                    consumer_name, kind
                );
            }
        }
    }
}
#[async_trait]
impl Actor for ClusterConsumerSupervisor {
    type Msg = SupervisorMessage;
    type State = ClusterConsumerSupervisorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let graph_config = get_neo_config()?;
        let events_output = std::sync::Arc::new(ractor::OutputPort::default());

        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        let client_config = TCPClientConfig::new()?;

        let (broker_client, _) = Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config: client_config,
                registration_id: None,
                events_output,
            },
            myself.clone().into(),
        )
        .await?;

        let state = ClusterConsumerSupervisorState {
            graph_config,
            broker_client,
        };

        Ok(state)
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::ClientEvent { event } => {
                match event {
                    ClientEvent::Registered { .. } => {
                        // try to conect to the graph
                        let graph = Graph::connect(state.graph_config.clone())?;

                        let (graph_controller, _) = Actor::spawn_linked(
                            Some("kubernetes.cluster.graph.controller".to_string()),
                            ClusterGraphController,
                            graph,
                            myself.clone().into(),
                        )
                        .await?;

                        //start actors
                        let args = KubeConsumerArgs {
                            broker_client: state.broker_client.clone(),
                            graph_controller,
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
                    ClientEvent::MessagePublished { topic, payload } => {
                        ClusterConsumerSupervisor::deserialize_and_dispatch(topic, payload)
                    }
                    ClientEvent::TransportError { reason } => {
                        error!("Transport error occurred! {reason}");
                        myself.stop(Some(reason))
                    }
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
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                // we no actors start w/o names
                let actor_name = actor_cell.get_name().unwrap();

                warn!(
                    "Consumer_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_name,
                    actor_cell.get_id()
                );
            }

            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }

        Ok(())
    }
}
