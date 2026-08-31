use crate::BROKER_CLIENT_NAME;
use crate::GraphOperable;
use cassini_client::OfflineBehavior;
use cassini_client::PublishRequest;
use cassini_types::ClientEvent;
use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{Namespace, Pod};
use kube_common::KUBERNETES_DISCOVERY_QUERY;
use kube_common::RawKubeEvent;
use kube_common::{
    KIND_KUSTOMIZATION, KIND_OCI_REPOSITORY, KUBERNETES_DISCOVERY_ANNOUNCE,
    RESOURCE_APPLIED_ACTION, RESOURCE_DELETED_ACTION,
    flux::{kustomization::Kustomization, oci_repositories::OciRepository},
};
use polar::DiscoverySourceRef;
use polar::ProvenanceEvent;
use polar::RkyvError;
use polar::SupervisorMessage;
use polar::cassini::CassiniClient;
use polar::cassini::SubscribeRequest;
use polar::cassini::TcpClient;
use polar::graph::controller::{
    GraphController, GraphControllerActor, GraphControllerArgs, GraphControllerMsg, GraphOp,
    GraphSignal, IntoGraphKey,
};
use polar::graph::nodes::builds::ArtifactNodeKey;
use polar::graph::nodes::kube::KubeNodeKey;
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use polar::topics::KUBERNETES_RESOLUTION_EVENTS;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use ractor::async_trait;
use ractor::concurrency::Duration;
use ractor::{Actor, OutputPort};
use serde::de::DeserializeOwned;
use serde_json::from_value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateFingerprint {
    pub signature: String,
    pub valid_from: String,
    pub last_state_node_id: Option<String>,
}

#[derive(Default)]
pub struct ProjectionCache {
    entries: HashMap<(String, String, String), StateFingerprint>, // (kind, cluster_uid, uid)
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
        cluster_uid: String,
        uid: String,
        new_signature: String,
        valid_from: String,
    ) -> EmitDecision {
        let key = (kind.clone(), cluster_uid, uid.clone());

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

    pub fn set_last_state_node_id(
        &mut self,
        kind: &str,
        cluster_uid: String,
        uid: &str,
        node_id: String,
    ) {
        if let Some(entry) = self
            .entries
            .get_mut(&(kind.to_string(), cluster_uid, uid.to_string()))
        {
            entry.last_state_node_id = Some(node_id);
        }
    }

    pub fn evict(&mut self, kind: String, cluster_uid: String, uid: &str) {
        self.entries.remove(&(kind, cluster_uid, uid.to_string()));
    }
}

enum InboundTopic {
    KubeEvents { cluster_uid: String },
    ResolutionEvents,
    DiscoveryAnnounce,
    Unknown,
}

fn classify_topic(topic: &str) -> InboundTopic {
    if topic == KUBERNETES_RESOLUTION_EVENTS {
        return InboundTopic::ResolutionEvents;
    }
    if topic == KUBERNETES_DISCOVERY_ANNOUNCE {
        return InboundTopic::DiscoveryAnnounce;
    }
    if let Some(cluster_uid) = topic
        .strip_prefix("polar.kubernetes.")
        .and_then(|rest| rest.strip_suffix(".events"))
    {
        return InboundTopic::KubeEvents {
            cluster_uid: cluster_uid.to_string(),
        };
    }
    InboundTopic::Unknown
}
pub struct ClusterConsumerSupervisor;

pub struct ClusterConsumerSupervisorState {
    broker_client: TcpClient,
    graph_controller: Option<GraphController>,
    projection_cache: ProjectionCache,
    healthcheck: ActorRef<HealthCheckMessage>,
    known_cluster_uids: HashSet<String>,
    /// Set true once a fatal failure has triggered the bounded drain window.
    /// While true, incoming messages are logged instead of processed, since
    /// whatever failed (likely the graph controller) can't be trusted, and
    /// we're exiting shortly regardless.
    draining: bool,
}

impl ClusterConsumerSupervisor {
    pub fn handle_event<T>(
        ev: RawKubeEvent,
        cache: &mut ProjectionCache,
        graph_controller: &GraphController,
        tcp_client: &TcpClient,
        cluster_uid: &str,
    ) -> Result<(), ActorProcessingErr>
    where
        T: DeserializeOwned + GraphOperable,
    {
        // mismatch check is cheap and worth having because ev.cluster_uid and the cluster derived from the subscribed topic name should always agree by construction
        // the observer stamps both from the same local value
        if ev.cluster_uid != cluster_uid {
            warn!(
                "cluster_uid mismatch: message arrived on the topic for cluster {cluster_uid} \
                 but its own payload claims {} -- trusting the topic, not the payload",
                ev.cluster_uid
            );
        }
        debug!("Handling event for resource {}", ev.kind);
        let obj = from_value::<T>(ev.object)?;
        match ev.action.as_str() {
            RESOURCE_APPLIED_ACTION => {
                obj.project_into_graph(graph_controller, tcp_client, cluster_uid, cache)?
            }
            RESOURCE_DELETED_ACTION => obj.project_delete(graph_controller, cluster_uid)?,
            _ => warn!("Unexpected action received!! {}", ev.action),
        }
        Ok(())
    }

    fn deserialize_and_dispatch(
        topic: String,
        payload: Vec<u8>,
        cache: &mut ProjectionCache,
        graph_controller: &GraphController,
        tcp_client: &TcpClient,
        known_cluster_uids: &mut HashSet<String>,
    ) -> Result<(), ActorProcessingErr> {
        match classify_topic(&topic) {
            InboundTopic::KubeEvents { cluster_uid } => {
                let ev: RawKubeEvent = serde_json::from_slice(&payload)?;
                match ev.kind.as_str() {
                    "Namespace" => Self::handle_event::<Namespace>(
                        ev,
                        cache,
                        graph_controller,
                        tcp_client,
                        &cluster_uid,
                    )?,
                    "Pod" => Self::handle_event::<Pod>(
                        ev,
                        cache,
                        graph_controller,
                        tcp_client,
                        &cluster_uid,
                    )?,
                    "Deployment" => Self::handle_event::<Deployment>(
                        ev,
                        cache,
                        graph_controller,
                        tcp_client,
                        &cluster_uid,
                    )?,
                    "ReplicaSet" => Self::handle_event::<ReplicaSet>(
                        ev,
                        cache,
                        graph_controller,
                        tcp_client,
                        &cluster_uid,
                    )?,
                    "Job" => Self::handle_event::<Job>(
                        ev,
                        cache,
                        graph_controller,
                        tcp_client,
                        &cluster_uid,
                    )?,
                    KIND_OCI_REPOSITORY => Self::handle_event::<OciRepository>(
                        ev,
                        cache,
                        graph_controller,
                        tcp_client,
                        &cluster_uid,
                    )?,
                    KIND_KUSTOMIZATION => Self::handle_event::<Kustomization>(
                        ev,
                        cache,
                        graph_controller,
                        tcp_client,
                        &cluster_uid,
                    )?,
                    _ => warn!("Unexpected resource type {}", ev.kind),
                }
            }
            InboundTopic::ResolutionEvents => {
                let ev = rkyv::from_bytes::<ProvenanceEvent, RkyvError>(&payload)?;
                if let ProvenanceEvent::OCIArtifactResolved {
                    digest, source_ref, ..
                } = ev
                {
                    if let DiscoverySourceRef::KubernetesPodContainer {
                        pod_uid,
                        container_name,
                        cluster_uid,
                        ..
                    } = source_ref
                    {
                        debug!(
                            "pod {pod_uid} container {container_name} resolved with digest {digest}, updating graph"
                        );
                        let container_k = KubeNodeKey::PodContainer {
                            pod_uid,
                            name: container_name,
                            cluster_uid,
                        };
                        let artifact_k = ArtifactNodeKey::OCIArtifact { digest };
                        let op = GraphOp::EnsureEdge {
                            from: container_k.into_key(),
                            to: artifact_k.into_key(),
                            rel_type: "USES_IMAGE".to_string(),
                            props: vec![],
                        };
                        graph_controller.send_message(GraphControllerMsg::Op(op))?;
                    }
                }
            }
            InboundTopic::DiscoveryAnnounce => {
                let announced_uid = String::from_utf8(payload).map_err(|e| {
                    ActorProcessingErr::from(format!("non-utf8 discovery.announce payload: {e}"))
                })?;
                if known_cluster_uids.insert(announced_uid.clone()) {
                    info!("Discovered cluster {announced_uid}, subscribing to its events topic");
                    tcp_client.subscribe(SubscribeRequest {
                        topic: kube_common::kube_events_topic(&announced_uid),
                        trace_ctx: None,
                    })?;
                } else {
                    debug!("Redundant announcement for known cluster {announced_uid}, no-op");
                }
            }
            InboundTopic::Unknown => warn!("Unexpected topic received: {topic}"),
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

        // Build the OutputPort that GraphControllerActor fires on every
        // real op's outcome (canary at spawn, and every subsequent write).
        let graph_signal_port = Arc::new(OutputPort::<GraphSignal>::default());
        graph_signal_port.subscribe(myself.clone(), |signal| {
            Some(SupervisorMessage::GraphSignal(signal))
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
                draining: false,
                known_cluster_uids: HashSet::new(),
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

                if let Err(e) = state.healthcheck.cast(HealthCheckMessage::ShutdownAck) {
                    error!("CONSUMER_SUPERVISOR: failed to send ShutdownAck: {e}");
                }

                // PrepareShutdown:
                if let Err(e) =
                    state
                        .broker_client
                        .unsubscribe(polar::cassini::UnsubscribeRequest {
                            topic: KUBERNETES_DISCOVERY_ANNOUNCE.to_string(),
                            trace_ctx: None,
                        })
                {
                    warn!("CONSUMER_SUPERVISOR: unsubscribe failed (continuing): {e}");
                }
                // Registered subscribes to this alongside discovery.announce
                // (see below); it was previously the only one of the three
                // subscriptions made there with no matching teardown here.
                if let Err(e) =
                    state
                        .broker_client
                        .unsubscribe(polar::cassini::UnsubscribeRequest {
                            topic: KUBERNETES_RESOLUTION_EVENTS.to_string(),
                            trace_ctx: None,
                        })
                {
                    warn!(
                        "CONSUMER_SUPERVISOR: unsubscribe from resolution events failed (continuing): {e}"
                    );
                }
                for cluster_uid in &state.known_cluster_uids {
                    if let Err(e) =
                        state
                            .broker_client
                            .unsubscribe(polar::cassini::UnsubscribeRequest {
                                topic: kube_common::kube_events_topic(cluster_uid),
                                trace_ctx: None,
                            })
                    {
                        warn!(
                            "CONSUMER_SUPERVISOR: unsubscribe failed for cluster {cluster_uid} (continuing): {e}"
                        );
                    }
                }
            }

            SupervisorMessage::GraphSignal(signal) => match signal {
                GraphSignal::Available => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::GraphAvailable);
                }
                GraphSignal::OpFailed(reason) => {
                    let _ = state
                        .healthcheck
                        .cast(HealthCheckMessage::GraphOpFailed(reason));
                }
            },

            SupervisorMessage::ForceExit => {
                warn!("CONSUMER_SUPERVISOR: drain window elapsed, exiting now");
                std::process::exit(1);
            }

            SupervisorMessage::ClientEvent { event } => {
                match event {
                    ClientEvent::Registered { .. } => {
                        let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);

                        if state.graph_controller.is_none() {
                            let graph_signal_port = Arc::new(OutputPort::<GraphSignal>::default());
                            graph_signal_port.subscribe(myself.clone(), |signal| {
                                Some(SupervisorMessage::GraphSignal(signal))
                            });

                            match GraphControllerActor::spawn_linked(
                                Some("kubernetes.cluster.graph.controller".to_string()),
                                GraphControllerActor,
                                GraphControllerArgs {
                                    signal_port: graph_signal_port,
                                },
                                myself.get_cell(),
                            )
                            .await
                            {
                                Ok((graph_controller, _)) => {
                                    state.graph_controller = Some(graph_controller);
                                    let _ =
                                        state.healthcheck.cast(HealthCheckMessage::GraphConnected);
                                }
                                Err(e) => {
                                    error!("Error initializing graph controller! {e}");
                                    return Err(ActorProcessingErr::from(e));
                                }
                            }
                        } else {
                            let _ = state.healthcheck.cast(HealthCheckMessage::GraphConnected);
                        }

                        info!("Subscribing to topics...");

                        if let Err(e) = state.broker_client.subscribe(SubscribeRequest {
                            topic: KUBERNETES_DISCOVERY_ANNOUNCE.into(),
                            trace_ctx: None,
                        }) {
                            error!("{e}");
                            return Err(ActorProcessingErr::from(e.to_string()));
                        }

                        if let Err(e) = state.broker_client.subscribe(SubscribeRequest {
                            topic: KUBERNETES_RESOLUTION_EVENTS.into(),
                            trace_ctx: None,
                        }) {
                            error!("{e}");
                            return Err(ActorProcessingErr::from(e.to_string()));
                        }

                        info!(
                            "Requesting re-announcement from any already-running kube-observers..."
                        );
                        if let Err(e) = state.broker_client.publish(PublishRequest {
                            topic: KUBERNETES_DISCOVERY_QUERY.to_string(),
                            trace_ctx: None,
                            payload: Vec::new(),
                            offline_behavior: OfflineBehavior::default(),
                        }) {
                            error!("{e}");
                            return Err(ActorProcessingErr::from(e.to_string()));
                        }
                    }

                    ClientEvent::MessagePublished { topic, payload, .. } => {
                        if state.draining {
                            warn!(
                                "CONSUMER_SUPERVISOR: draining -- logging message on topic \
                                 '{topic}' ({} bytes) instead of processing; will be lost on exit",
                                payload.len()
                            );
                            // TODO: publish to dead-letter queue once Cassini
                            // supports one, instead of only logging.
                        } else if let Some(controller) = &state.graph_controller {
                            Self::deserialize_and_dispatch(
                                topic,
                                payload,
                                &mut state.projection_cache,
                                controller,
                                &state.broker_client,
                                &mut state.known_cluster_uids,
                            )?;
                        } else {
                            error!("No graph controller present!");
                            myself.stop(None);
                        }
                    }

                    ClientEvent::TransportError { reason } => {
                        error!("Transport error occurred! {reason}");
                        let _ = state
                            .healthcheck
                            .cast(HealthCheckMessage::CassiniDisconnected);
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
                let actor_name = actor_cell.get_name().unwrap_or_default();
                info!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_name,
                    actor_cell.get_id()
                );
                // Clean termination of the healthcheck actor means the
                // rejuvenation sequence completed -- begin the same bounded
                // drain window as a hard failure, rather than exiting
                // immediately, so any messages still arriving get logged.
                if actor_name == HEALTHCHECK_ACTOR_NAME && !state.draining {
                    state.draining = true;
                    info!(
                        "CONSUMER_SUPERVISOR: healthcheck actor terminated cleanly, \
                         entering drain window before exiting for rejuvenation"
                    );
                    let _ = myself
                        .send_after(Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                let actor_name = actor_cell.get_name().unwrap_or_default();
                error!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_name,
                    actor_cell.get_id()
                );
                // Any child actor failure is fatal, but don't exit
                // immediately -- give the mailbox loop a bounded window to
                // keep logging any in-flight messages (handle() branches on
                // state.draining above) before ForceExit actually exits.
                if !state.draining {
                    state.draining = true;
                    warn!(
                        "CONSUMER_SUPERVISOR: entering drain window before exit; \
                         any messages that arrive will be logged, not processed"
                    );
                    let _ = myself
                        .send_after(Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }

        Ok(())
    }
}
