use cassini_client::*;
use cassini_types::ClientEvent;
use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::core::v1::Pod;
use kube_common::{RawKubeEvent, KUBERNETES_CONSUMER};
use kube_common::{RESOURCE_APPLIED_ACTION, RESOURCE_DELETED_ACTION};
use neo4rs::{Config, Graph};
use polar::get_neo_config;
use polar::SupervisorMessage;
use ractor::async_trait;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use serde::de::DeserializeOwned;
use serde_json::from_value;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

use polar::graph::GraphController;
// use crate::pods::PodConsumer;
use crate::ClusterGraphController;
use crate::GraphOperable;
use crate::{KubeNodeKey, BROKER_CLIENT_NAME};

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateFingerprint {
    /// Canonical serialized state payload.
    /// Could be JSON, or stable key=value pairs.
    pub signature: String,

    /// The valid_from timestamp used when we emitted it.
    pub valid_from: String,
    pub last_state_node_id: Option<String>,
}
// TODO: Impl this?
// impl StateFingerprint {
//     /// More compact. Faster comparisons. Cleaner.
//     pub fn hash_signature(payload: &str) -> String {
//         let mut hasher = Sha256::new();
//         hasher.update(payload.as_bytes());
//         format!("{:x}", hasher.finalize())
//     }
// }

#[derive(Default)]
pub struct ProjectionCache {
    // key: (kind, uid)
    entries: HashMap<(String, String), StateFingerprint>,
}

pub enum EmitDecision {
    Suppress,
    Emit {
        previous_state_node_id: Option<String>,
    },
}

impl ProjectionCache {
    /// Returns true if this is a new state and should be emitted.
    pub fn should_emit(
        &mut self,
        kind: String,
        uid: String,
        new_signature: String,
        valid_from: String,
    ) -> EmitDecision {
        let key = (kind.clone(), uid.clone());

        match self.entries.get(&key) {
            Some(existing) if existing.signature == new_signature => EmitDecision::Suppress,
            Some(existing) => {
                let prev = existing.last_state_node_id.clone();

                self.entries.insert(
                    key,
                    StateFingerprint {
                        signature: new_signature,
                        valid_from,
                        last_state_node_id: None, // filled after graph write
                    },
                );

                EmitDecision::Emit {
                    previous_state_node_id: prev,
                }
            }
            None => {
                self.entries.insert(
                    key,
                    StateFingerprint {
                        signature: new_signature,
                        valid_from,
                        last_state_node_id: None,
                    },
                );

                EmitDecision::Emit {
                    previous_state_node_id: None,
                }
            }
        }
    }
    pub fn set_last_state_node_id(&mut self, kind: &str, uid: &str, node_id: String) {
        if let Some(entry) = self.entries.get_mut(&(kind.to_string(), uid.to_string())) {
            entry.last_state_node_id = Some(node_id);
        }
    }

    /// Remove from cache on terminal deletion if desired.
    pub fn evict(&mut self, kind: String, uid: &str) {
        self.entries.remove(&(kind, uid.to_string()));
    }
}

pub struct ClusterConsumerSupervisor;

pub struct ClusterConsumerSupervisorState {
    graph_config: Config,
    broker_client: ActorRef<TcpClientMessage>,
    graph_controller: Option<GraphController<KubeNodeKey>>,
    projection_cache: ProjectionCache,
}

impl ClusterConsumerSupervisor {
    pub fn handle_event<T>(
        ev: RawKubeEvent,
        _cache: &mut ProjectionCache,
        graph_controller: &GraphController<KubeNodeKey>,
        tcp_client: &TcpClient,
    ) -> Result<(), ActorProcessingErr>
    where
        T: DeserializeOwned + GraphOperable,
    {
        debug!("Handling event for resource {}", ev.kind);
        let obj = from_value::<T>(ev.object)?;

        match ev.action.as_str() {
            RESOURCE_APPLIED_ACTION => {
                debug!("handling RESOURCE_APPLIED_ACTION.");
                obj.project_into_graph(graph_controller, tcp_client)?
            }
            RESOURCE_DELETED_ACTION => {
                debug!("handling RESOURCE_DELETED_ACTION.");
                obj.project_delete(graph_controller)?;
            }
            _ => todo!(),
        }
        Ok(())
    }

    fn deserialize_and_dispatch(
        _topic: String,
        payload: Vec<u8>,
        cache: &mut ProjectionCache,
        graph_controller: &GraphController<KubeNodeKey>,
        tcp_client: &TcpClient,
    ) -> Result<(), ActorProcessingErr> {
        // 1) Parse the raw message into your RawKubeEvent
        let ev: RawKubeEvent = serde_json::from_slice(&payload)?;

        match ev.kind.as_str() {
            "Pod" => Self::handle_event::<Pod>(ev, cache, graph_controller, tcp_client)?,
            "Deployment" => {
                Self::handle_event::<Deployment>(ev, cache, graph_controller, tcp_client)?
            }
            "ReplicaSet" => {
                Self::handle_event::<ReplicaSet>(ev, cache, graph_controller, tcp_client)?
            }
            "Node" => todo!("Nodes"),
            _ => warn!("Unexpected resource type {}", ev.kind),
        }

        Ok(())
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
                events_output: Some(events_output),
                event_handler: None,
            },
            myself.clone().into(),
        )
        .await?;

        let state = ClusterConsumerSupervisorState {
            graph_config,
            broker_client,
            graph_controller: None,
            projection_cache: ProjectionCache {
                entries: HashMap::new(),
            },
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

                        state.graph_controller = Actor::spawn_linked(
                            Some("kubernetes.cluster.graph.controller".to_string()),
                            ClusterGraphController,
                            graph,
                            myself.clone().into(),
                        )
                        .await?
                        .0
                        .into();

                        // subscribe
                        //
                        if let Err(e) = state
                            .broker_client
                            .cast(TcpClientMessage::Subscribe {
                                topic: KUBERNETES_CONSUMER.into(),
                                trace_ctx: None,
                            })
                        {
                            error!("{e}");
                            myself.stop(None);
                        }
                    }
                    ClientEvent::MessagePublished { topic, payload, .. } => {
                        if let Some(controller) = &state.graph_controller {
                            Self::deserialize_and_dispatch(
                                topic,
                                payload,
                                &mut state.projection_cache,
                                &controller,
                                &state.broker_client,
                            )?;
                        } else {
                            error!("No graph controller present!");
                            myself.stop(None);
                        }
                    }
                    ClientEvent::TransportError { reason } => {
                        error!("Transport error occurred! {reason}");
                        myself.stop(Some(reason))
                    }
                    ClientEvent::ControlResponse { .. } => {
                        // ignore or log
                        debug!("Ignoring ControlResponse in consumer supervisor");
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
