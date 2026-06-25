use crate::BROKER_CLIENT_NAME;
use crate::GraphOperable;
use cassini_types::ClientEvent;
use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use kube_common::{
    KIND_KUSTOMIZATION, KIND_OCI_REPOSITORY, RESOURCE_APPLIED_ACTION, RESOURCE_DELETED_ACTION,
    flux::{kustomization::Kustomization, oci_repositories::OciRepository},
};
use kube_common::{KUBERNETES_CONSUMER, RawKubeEvent};
use polar::SupervisorMessage;
use polar::cassini::CassiniClient;
use polar::cassini::SubscribeRequest;
use polar::cassini::TcpClient;
use polar::graph::controller::{GraphController, GraphControllerActor};
use polar::health::{
    DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage,
};
use ractor::{Actor, OutputPort};
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use ractor::async_trait;
use serde::de::DeserializeOwned;
use serde_json::from_value;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateFingerprint {
    pub signature: String,
    pub valid_from: String,
    pub last_state_node_id: Option<String>,
}

#[derive(Default)]
pub struct ProjectionCache {
    entries: HashMap<(String, String), StateFingerprint>,
}

pub enum EmitDecision {
    Suppress,
    Emit {
        previous_state_node_id: Option<String>,
    },
}

impl ProjectionCache {
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
                        last_state_node_id: None,
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

    pub fn evict(&mut self, kind: String, uid: &str) {
        self.entries.remove(&(kind, uid.to_string()));
    }
}

pub struct ClusterConsumerSupervisor;

pub struct ClusterConsumerSupervisorState {
    broker_client: TcpClient,
    graph_controller: Option<GraphController>,
    projection_cache: ProjectionCache,
    healthcheck: ActorRef<HealthCheckMessage>,
}

impl ClusterConsumerSupervisor {
    pub fn handle_event<T>(
        ev: RawKubeEvent,
        _cache: &mut ProjectionCache,
        graph_controller: &GraphController,
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
        graph_controller: &GraphController,
        tcp_client: &TcpClient,
    ) -> Result<(), ActorProcessingErr> {
        let ev: RawKubeEvent = serde_json::from_slice(&payload)?;

        match ev.kind.as_str() {
            "Pod" => Self::handle_event::<Pod>(ev, cache, graph_controller, tcp_client)?,
            "Deployment" => {
                Self::handle_event::<Deployment>(ev, cache, graph_controller, tcp_client)?
            }
            "ReplicaSet" => {
                Self::handle_event::<ReplicaSet>(ev, cache, graph_controller, tcp_client)?
            }
            "Job" => Self::handle_event::<Job>(ev, cache, graph_controller, tcp_client)?,
            "Node" => todo!("Nodes"),
            KIND_OCI_REPOSITORY => {
                Self::handle_event::<OciRepository>(ev, cache, graph_controller, tcp_client)?
            }
            KIND_KUSTOMIZATION => {
                Self::handle_event::<Kustomization>(ev, cache, graph_controller, tcp_client)?
            }
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
        info!("Read neo configuration successfully.");

        // Build the OutputPort that HealthCheckActor fires when it wants
        // the supervisor to prepare for shutdown. We subscribe here so
        // that the message arrives as a SupervisorMessage::PrepareShutdown.
        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        // Parse dep cert endpoints for this agent:
        // kube-consumer depends on Cassini and Neo4j.
        let dep_cert_endpoints = vec![
            DepCertEndpoint::parse("cassini-ip-svc.polar.svc.cluster.local:8080:300")
                .map_err(|e| ActorProcessingErr::from(e))?,
            DepCertEndpoint::parse("polar-db-svc.polar-graph.svc.cluster.local:7687:300")
                .map_err(|e| ActorProcessingErr::from(e))?,
        ];

        let (healthcheck, _) = HealthCheckActor::spawn_linked(
            Some(HEALTHCHECK_ACTOR_NAME.to_string()),
            HealthCheckActor,
            HealthCheckArgs {
                expects_graph: true,
                rejuvenation_threshold_secs: 300,
                dep_cert_endpoints,
                prepare_shutdown_port,
            },
            myself.get_cell(),
        )
        .await
        .map_err(|e| ActorProcessingErr::from(e))?;

        match TcpClient::spawn(BROKER_CLIENT_NAME, myself, |event| {
            Some(SupervisorMessage::ClientEvent { event })
        })
        .await
        {
            Ok(broker_client) => Ok(ClusterConsumerSupervisorState {
                broker_client,
                graph_controller: None,
                projection_cache: ProjectionCache {
                    entries: HashMap::new(),
                },
                healthcheck,
            }),
            Err(e) => Err(ActorProcessingErr::from(e)),
        }
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::PrepareShutdown => {
                info!("CONSUMER_SUPERVISOR: PrepareShutdown received, unsubscribing from Cassini");

                // Unsubscribe from the topic — no new messages will arrive
                // after this. Because ractor is sequential, any messages
                // already in our mailbox ahead of PrepareShutdown have
                // already been processed.
                if let Err(e) = state.broker_client.unsubscribe(polar::cassini::UnsubscribeRequest {
                    topic: KUBERNETES_CONSUMER.to_string(),
                    trace_ctx: None,
                }) {
                    warn!("CONSUMER_SUPERVISOR: unsubscribe failed (continuing): {e}");
                }

                // Acknowledge to the HealthCheckActor so it can stop cleanly.
                if let Err(e) = state.healthcheck.cast(HealthCheckMessage::ShutdownAck) {
                    error!("CONSUMER_SUPERVISOR: failed to send ShutdownAck: {e}");
                }
            }

            SupervisorMessage::ClientEvent { event } => {
                match event {
                    ClientEvent::Registered { .. } => {
                        let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);

                        // Only spawn graph controller if not already present
                        if state.graph_controller.is_none() {
                            match GraphControllerActor::spawn_linked(
                                Some("kubernetes.cluster.graph.controller".to_string()),
                                GraphControllerActor,
                                (),
                                myself.get_cell(),
                            )
                            .await
                            {
                                Ok((graph_controller, _)) => {
                                    state.graph_controller = Some(graph_controller);
                                    let _ = state
                                        .healthcheck
                                        .cast(HealthCheckMessage::GraphConnected);
                                }
                                Err(e) => {
                                    error!("Error initializing graph controller! {e}");
                                    return Err(ActorProcessingErr::from(e));
                                }
                            }
                        } else {
                            // Already have a graph controller — just notify healthcheck
                            let _ = state.healthcheck.cast(HealthCheckMessage::GraphConnected);
                        }

                        // Always re-subscribe on reconnect
                        info!("Subscribing to topics...");
                        if let Err(e) = state.broker_client.subscribe(SubscribeRequest {
                            topic: KUBERNETES_CONSUMER.into(),
                            trace_ctx: None,
                        }) {
                            error!("{e}");
                            return Err(ActorProcessingErr::from(e.to_string()));
                        }
                    }

                    ClientEvent::MessagePublished { topic, payload, .. } => {
                        if let Some(controller) = &state.graph_controller {
                            Self::deserialize_and_dispatch(
                                topic,
                                payload,
                                &mut state.projection_cache,
                                controller,
                                &state.broker_client,
                            )?;
                        } else {
                            error!("No graph controller present!");
                            myself.stop(None);
                        }
                    }

                    ClientEvent::TransportError { reason } => {
                        error!("Transport error occurred! {reason}");
                        let _ = state.healthcheck.cast(HealthCheckMessage::CassiniDisconnected);
                        // Don't stop — the TCP client will attempt to reconnect.
                        // HealthCheckActor tracks the disconnected state and will
                        // fail if reconnection doesn't happen within the tick interval.
                    }

                    _ => (),
                }
            }

            SupervisorMessage::Heartbeat => {}
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
                let actor_name = actor_cell.get_name().unwrap_or_default();
                info!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_name,
                    actor_cell.get_id()
                );
                // Clean termination of the healthcheck actor means the
                // rejuvenation sequence completed — exit so the Job
                // controller creates a new pod with fresh certs.
                if actor_name == HEALTHCHECK_ACTOR_NAME {
                    info!("CONSUMER_SUPERVISOR: healthcheck actor terminated cleanly, exiting for rejuvenation");
                    std::process::exit(1);
                }
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                let actor_name = actor_cell.get_name().unwrap_or_default();
                error!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_name,
                    actor_cell.get_id()
                );
                // Any child actor failure is fatal — exit so the Job
                // controller creates a new pod with fresh certs.
                std::process::exit(1);
            }
            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }

        Ok(())
    }
}
