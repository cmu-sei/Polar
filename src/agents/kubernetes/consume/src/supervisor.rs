use crate::BROKER_CLIENT_NAME;
use crate::GraphOperable;
use cassini_client::TcpClientMessage;
use cassini_types::ClientEvent;
use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use kube_common::{
    KIND_KUSTOMIZATION, KIND_OCI_REPOSITORY, RESOURCE_APPLIED_ACTION, RESOURCE_DELETED_ACTION,
    flux::{kustomization::Kustomization, oci_repositories::OciRepository},
};
use kube_common::{KUBERNETES_CONSUMER, RawKubeEvent};
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
use ractor::{Actor, OutputPort};
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use ractor::async_trait;
use ractor::concurrency::Duration;
use serde::de::DeserializeOwned;
use serde_json::from_value;
use std::collections::HashMap;
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
    /// The real, first, genuine CryptoActor user -- shares this
    /// supervisor's own, already-established Cassini connection and
    /// RouterActor rather than opening a second connection. Not yet
    /// consumed by anything in this file's own, real projection
    /// logic; the real, upcoming, plaintext-Kubernetes-secret
    /// encryption work (see this crate's own, real design
    /// discussion) is the actual, first real caller.
    crypto_actor: ActorRef<polar_crypto::CryptoMessage>,
    /// Set true once a fatal failure has triggered the bounded drain window.
    /// While true, incoming messages are logged instead of processed, since
    /// whatever failed (likely the graph controller) can't be trusted, and
    /// we're exiting shortly regardless.
    draining: bool,
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
    ) -> Result<(), ActorProcessingErr> {
        // determine the topic the mesasge came from, we could've gotten notified of a new resource, or been notified that
        // some resource we saw was resolved from an external source (like a container image)
        match topic.as_str() {
            KUBERNETES_CONSUMER => {
                // 1) Parse the raw message into your RawKubeEvent
                let ev: RawKubeEvent = serde_json::from_slice(&payload)?;

                // TODO: Define constants for these and match on them instead, these literals are also used in the marco calls to define their watchers
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
                    KIND_OCI_REPOSITORY => Self::handle_event::<OciRepository>(
                        ev,
                        cache,
                        graph_controller,
                        tcp_client,
                    )?,
                    KIND_KUSTOMIZATION => Self::handle_event::<Kustomization>(
                        ev,
                        cache,
                        graph_controller,
                        tcp_client,
                    )?,
                    _ => warn!("Unexpected resource type {}", ev.kind),
                }
            }
            KUBERNETES_RESOLUTION_EVENTS => {
                let ev = rkyv::from_bytes::<ProvenanceEvent, RkyvError>(&payload)?;
                match ev {
                    ProvenanceEvent::OCIArtifactResolved {
                        digest, source_ref, ..
                    } => {
                        if let DiscoverySourceRef::KubernetesPodContainer {
                            pod_uid,
                            container_name,
                            ..
                        } = source_ref
                        {
                            debug!(
                                "pod {pod_uid} container {container_name} was resolved with digest {digest}, updating graph"
                            );
                            let container_k = KubeNodeKey::PodContainer {
                                pod_uid,
                                name: container_name,
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
                    _ => (), //ignore all other events
                }
            }
            _ => (), //ignore unhandled topics for now, we shouldn't see anything else
        }
        Ok(())
    }

    /// Registers this agent's own two, real, genuinely-owned topics
    /// (KUBERNETES_CONSUMER, KUBERNETES_RESOLUTION_EVENTS) with
    /// `router`. Each topic's own, real deliver closure deliberately
    /// does the minimal, correct thing -- reconstruct the exact same
    /// ClientEvent::MessagePublished this agent's own, existing,
    /// unchanged handle()/deserialize_and_dispatch logic already
    /// expects, and forward it to `supervisor` as the same, existing
    /// SupervisorMessage::ClientEvent it already handles. Extracted
    /// from pre_start specifically so a real test can exercise this
    /// registration/dispatch behavior directly, without needing the
    /// real network connection pre_start also, separately, attempts.
    fn register_kube_topics(
        router: &ActorRef<polar_router::RouterMessage>,
        supervisor: &ActorRef<SupervisorMessage>,
    ) -> Result<(), ActorProcessingErr> {
        for topic in [KUBERNETES_CONSUMER, KUBERNETES_RESOLUTION_EVENTS] {
            let supervisor_for_deliver = supervisor.clone();
            router
                .send_message(polar_router::RouterMessage::Register {
                    topic: topic.to_string(),
                    deliver: Box::new(move |incoming: polar_router::IncomingMessage| {
                        let event = ClientEvent::MessagePublished {
                            topic: incoming.topic,
                            payload: incoming.payload,
                            trace_ctx: incoming.trace_ctx,
                            published_by: incoming.published_by,
                        };
                        if let Err(e) = supervisor_for_deliver
                            .send_message(SupervisorMessage::ClientEvent { event })
                        {
                            warn!("RouterActor: failed to deliver routed message to supervisor: {e}");
                        }
                    }),
                })
                .map_err(ActorProcessingErr::from)?;
        }
        Ok(())
    }

    /// Spawns this agent's own, real, first, genuine CryptoActor
    /// user, registering its own five, real reply topics against the
    /// same, shared `router` this agent's own two, real topics are
    /// already registered with (see register_kube_topics, above).
    /// Extracted from pre_start specifically so a real test can
    /// exercise this exact, real spawn sequence directly, without
    /// needing the real network connection pre_start also,
    /// separately, attempts for broker_client itself.
    async fn spawn_crypto_actor(
        router: ActorRef<polar_router::RouterMessage>,
        tcp_client: ActorRef<TcpClientMessage>,
        supervisor: ractor::ActorCell,
    ) -> Result<ActorRef<polar_crypto::CryptoMessage>, ActorProcessingErr> {
        let (crypto_actor, _) = polar_crypto::CryptoActor::spawn_linked(
            None,
            polar_crypto::CryptoActor,
            polar_crypto::CryptoActorArgs { tcp_client, router },
            supervisor,
        )
        .await
        .map_err(ActorProcessingErr::from)?;
        Ok(crypto_actor)
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

        // A real, first, proof-of-concept RouterActor integration --
        // see polar-router's own crate documentation for the complete
        // design. RouterActor itself never touches this supervisor's
        // own, private, mutable state (graph_controller,
        // projection_cache) directly, since a registered closure runs
        // inside RouterActor's own actor context, not this one's --
        // see register_kube_topics's own doc comment for what each
        // topic's own, real deliver closure does instead.
        let (router, _) = polar_router::RouterActor::spawn_linked(
            None,
            polar_router::RouterActor,
            (),
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        Self::register_kube_topics(&router, &myself)?;

        // Captured before myself is moved into spawn_with_router
        // below (it takes its own supervisor argument by value) --
        // needed afterward for spawn_crypto_actor's own, real
        // spawn_linked call.
        let myself_cell = myself.get_cell();

        let broker_client = TcpClient::spawn_with_router(
            BROKER_CLIENT_NAME,
            myself,
            |event| Some(SupervisorMessage::ClientEvent { event }),
            router.clone(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        // CryptoActor shares this same, real, already-established
        // Cassini connection and RouterActor -- spawned only now,
        // after broker_client itself genuinely exists, since
        // CryptoActorArgs needs its own, real, raw
        // ActorRef<TcpClientMessage> (as_actor_ref()), not available
        // any earlier in this function.
        let crypto_actor =
            Self::spawn_crypto_actor(router, broker_client.as_actor_ref(), myself_cell).await?;

        Ok(ClusterConsumerSupervisorState {
            broker_client,
            graph_controller: None,
            projection_cache: ProjectionCache {
                entries: HashMap::new(),
            },
            healthcheck,
            crypto_actor,
            draining: false,
        })
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

                if let Err(e) = state.broker_client.unsubscribe(polar::cassini::UnsubscribeRequest {
                    topic: KUBERNETES_CONSUMER.to_string(),
                    trace_ctx: None,
                }) {
                    warn!("CONSUMER_SUPERVISOR: unsubscribe failed (continuing): {e}");
                }

                if let Err(e) = state.healthcheck.cast(HealthCheckMessage::ShutdownAck) {
                    error!("CONSUMER_SUPERVISOR: failed to send ShutdownAck: {e}");
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
                warn!("CONSUMER_SUPERVISOR: drain window elapsed, beginning graceful shutdown");

                if let Some(controller) = state.graph_controller.take() {
                    warn!(
                        "CONSUMER_SUPERVISOR: stopping graph controller and waiting for any \
                         in-flight write to complete before exiting"
                    );
                    match controller
                        .stop_and_wait(
                            Some("supervisor shutting down".into()),
                            Some(Duration::from_secs(30)),
                        )
                        .await
                    {
                        Ok(()) => {
                            info!("CONSUMER_SUPERVISOR: graph controller stopped cleanly");
                        }
                        Err(e) => {
                            error!(
                                "CONSUMER_SUPERVISOR: graph controller did not stop cleanly \
                                 within 30s, exiting anyway: {e:?}"
                            );
                        }
                    }
                }

                warn!("CONSUMER_SUPERVISOR: exiting now");
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
                                GraphControllerArgs { signal_port: graph_signal_port },
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
                            let _ = state.healthcheck.cast(HealthCheckMessage::GraphConnected);
                        }

                        info!("Subscribing to topics...");
                        if let Err(e) = state.broker_client.subscribe(SubscribeRequest {
                            topic: KUBERNETES_CONSUMER.into(),
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
                            )?;
                        } else {
                            error!("No graph controller present!");
                            myself.stop(None);
                        }
                    }

                    ClientEvent::TransportError { reason } => {
                        error!("Transport error occurred! {reason}");
                        let _ = state.healthcheck.cast(HealthCheckMessage::CassiniDisconnected);
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

#[cfg(test)]
mod router_integration_tests {
    use super::*;
    use ractor::{Actor, ActorProcessingErr, async_trait};
    use std::sync::{Arc, Mutex};

    /// A minimal, real stand-in for ClusterConsumerSupervisor itself
    /// -- genuinely receives SupervisorMessage, just records what it
    /// got rather than processing it, since this test's own, real
    /// purpose is proving RouterActor's delivery reaches a real actor
    /// correctly, not re-testing this crate's own, separate,
    /// unchanged handle()/deserialize_and_dispatch logic.
    struct FakeSupervisor {
        received: Arc<Mutex<Option<ClientEvent>>>,
    }

    #[async_trait]
    impl Actor for FakeSupervisor {
        type Msg = SupervisorMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let SupervisorMessage::ClientEvent { event } = message {
                *self.received.lock().unwrap() = Some(event);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_message_routed_on_kubernetes_consumer_genuinely_reaches_the_supervisor() {
        // The real, end-to-end proof this integration is meant to
        // provide: a raw IncomingMessage on KUBERNETES_CONSUMER,
        // routed through a real RouterActor via the exact,
        // production registration function, reconstructed as the
        // exact same ClientEvent::MessagePublished this agent's own,
        // real handle() already expects, and genuinely delivered to
        // the real actor that registered for it.
        let received = Arc::new(Mutex::new(None));
        let (fake_supervisor, _) = Actor::spawn(
            None,
            FakeSupervisor {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let (router, _) = polar_router::RouterActor::spawn(None, polar_router::RouterActor, ())
            .await
            .unwrap();

        ClusterConsumerSupervisor::register_kube_topics(&router, &fake_supervisor).unwrap();

        router
            .send_message(polar_router::RouterMessage::Route(
                polar_router::IncomingMessage {
                    topic: KUBERNETES_CONSUMER.to_string(),
                    payload: b"a real, raw kube event payload".to_vec(),
                    published_by: Some("kube-observer".to_string()),
                    trace_ctx: None,
                },
            ))
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let got = received
            .lock()
            .unwrap()
            .clone()
            .expect("the fake supervisor must have genuinely received a real ClientEvent");
        match got {
            ClientEvent::MessagePublished {
                topic,
                payload,
                published_by,
                ..
            } => {
                assert_eq!(topic, KUBERNETES_CONSUMER);
                assert_eq!(payload, b"a real, raw kube event payload");
                assert_eq!(published_by, Some("kube-observer".to_string()));
            }
            other => panic!("expected MessagePublished, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_message_on_an_unregistered_topic_never_reaches_the_supervisor() {
        // The real, other half of the proof: RouterActor's own,
        // genuine topic-scoping actually holds -- a message on a
        // topic this agent never registered for must never reach it
        // at all, not even accidentally.
        let received = Arc::new(Mutex::new(None));
        let (fake_supervisor, _) = Actor::spawn(
            None,
            FakeSupervisor {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let (router, _) = polar_router::RouterActor::spawn(None, polar_router::RouterActor, ())
            .await
            .unwrap();

        ClusterConsumerSupervisor::register_kube_topics(&router, &fake_supervisor).unwrap();

        router
            .send_message(polar_router::RouterMessage::Route(
                polar_router::IncomingMessage {
                    topic: "some-completely-unrelated-topic".to_string(),
                    payload: b"irrelevant".to_vec(),
                    published_by: None,
                    trace_ctx: None,
                },
            ))
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(received.lock().unwrap().is_none());
    }

    /// Intercepts CryptoActor's own outgoing publishes and answers
    /// them exactly as the real Credentials Agent would, publishing
    /// its own real replies back through the same, shared `router`
    /// CryptoActor itself registered its own reply topics with --
    /// mirroring polar-crypto's own, established FakeCredentialsAgent
    /// test helper exactly.
    struct FakeCredentialsAgent {
        router: ActorRef<polar_router::RouterMessage>,
        escrow_public_key_pem: String,
    }

    #[async_trait]
    impl Actor for FakeCredentialsAgent {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            let TcpClientMessage::Publish { topic, payload, .. } = message else {
                return Ok(());
            };

            let route = |reply_topic: &str, bytes: Vec<u8>| {
                let _ = self.router.send_message(polar_router::RouterMessage::Route(
                    polar_router::IncomingMessage {
                        topic: reply_topic.to_string(),
                        payload: bytes,
                        published_by: None,
                        trace_ctx: None,
                    },
                ));
            };

            if topic == credentials_agent_common::CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC {
                type AgentEscrowKeyRequestEnvelope = polar::request_reply::Envelope<credentials_agent_common::agent_escrow_key::AgentEscrowKeyRequest>;
                if let Ok(envelope) = rkyv::from_bytes::<AgentEscrowKeyRequestEnvelope, rkyv::rancor::Error>(&payload) {
                    let reply = polar::request_reply::Envelope {
                        correlation_id: envelope.correlation_id,
                        payload: credentials_agent_common::agent_escrow_key::AgentEscrowKeyReply::Found {
                            public_key_pem: self.escrow_public_key_pem.clone(),
                        },
                    };
                    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&reply).unwrap().to_vec();
                    route(
                        credentials_agent_common::CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC,
                        bytes,
                    );
                }
            }
            // Every other, real CryptoActor request topic is
            // deliberately left unanswered here -- this test's own,
            // real purpose is proving EncryptForRecipient genuinely
            // works through kube-consumer's own, exact, real spawn
            // sequence, not re-proving every one of CryptoActor's
            // own flows (already, separately proven in polar-crypto's
            // own test suite).

            Ok(())
        }
    }

    /// A real, temporary, self-signed certificate carrying
    /// "kube-consumer" as its own identity SAN -- CryptoActor's own
    /// pre_start now reads a real, mounted TLS_CLIENT_CERT to derive
    /// its own identity (see polar-crypto's own, real
    /// extract_own_identity-based redesign), so this real,
    /// end-to-end test needs one too, exactly the same, real way
    /// polar-crypto's own tests already do.
    fn write_test_agent_certificate() -> tempfile::NamedTempFile {
        let mut rng = rand::rng();
        let private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let private_key_pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(
            &private_key,
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();
        let key_pair = rcgen::KeyPair::from_pem(&private_key_pem).unwrap();
        let params = rcgen::CertificateParams::new(vec!["kube-consumer".to_string()]).unwrap();
        let certificate_pem = params.self_signed(&key_pair).unwrap().pem();

        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), certificate_pem).unwrap();
        file
    }

    #[tokio::test]
    async fn crypto_actor_genuinely_works_through_kube_consumers_own_real_spawn_sequence() {
        // The real, end-to-end proof this whole session's own
        // RouterActor/CryptoActor migration work exists for:
        // kube-consumer's own, exact, real spawn_crypto_actor
        // sequence -- the same, real function pre_start itself
        // calls -- genuinely produces a working CryptoActor, sharing
        // the same, real router this agent's own two, genuinely-
        // owned topics are also registered with, with no conflict
        // between the two, real sets of topics on that one, shared
        // router.

        let received = Arc::new(Mutex::new(None));
        let (fake_supervisor, _) = Actor::spawn(
            None,
            FakeSupervisor {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let (router, _) = polar_router::RouterActor::spawn(None, polar_router::RouterActor, ())
            .await
            .unwrap();

        // This agent's own, real, genuinely-owned topics, registered
        // first, exactly as pre_start itself does.
        ClusterConsumerSupervisor::register_kube_topics(&router, &fake_supervisor).unwrap();

        let mut rng = rand::rng();
        let escrow_private_key = rsa::RsaPrivateKey::new(&mut rng, 4096).unwrap();
        let escrow_public_key_pem = rsa::pkcs8::EncodePublicKey::to_public_key_pem(
            &escrow_private_key.to_public_key(),
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();

        let (fake_credentials_agent, _) = Actor::spawn(
            None,
            FakeCredentialsAgent {
                router: router.clone(),
                escrow_public_key_pem,
            },
            (),
        )
        .await
        .unwrap();

        let own_cert_file = write_test_agent_certificate();
        unsafe {
            std::env::set_var("TLS_CLIENT_CERT", own_cert_file.path());
        }

        // The actual, real function this test exists to exercise --
        // not a reimplementation of it.
        let crypto_actor = ClusterConsumerSupervisor::spawn_crypto_actor(
            router.clone(),
            fake_credentials_agent,
            fake_supervisor.get_cell(),
        )
        .await
        .unwrap();

        unsafe {
            std::env::remove_var("TLS_CLIENT_CERT");
        }
        drop(own_cert_file);

        // Real proof 1: CryptoActor itself genuinely works through
        // this exact, real spawn sequence -- a real
        // EncryptForRecipient round trip, decrypted with the
        // recipient's own, real private key.
        let plaintext = b"a real secret, encrypted through kube-consumer's own real wiring";
        let result = ractor::call!(crypto_actor, |reply| {
            polar_crypto::CryptoMessage::EncryptForRecipient {
                recipient_identity: "some-other-agent".to_string(),
                plaintext: plaintext.to_vec(),
                reply,
            }
        })
        .unwrap();
        let wire_bytes = result.expect("EncryptForRecipient must genuinely succeed");
        let escrowed: credentials_agent_common::escrow_secret::EscrowedSecret =
            rkyv::from_bytes::<_, rkyv::rancor::Error>(&wire_bytes).unwrap();
        let decrypted = escrowed.decrypt(&escrow_private_key).unwrap();
        assert_eq!(&*decrypted, plaintext);

        // Real proof 2: this agent's own, genuinely-owned topics
        // still, correctly reach the very same fake_supervisor above
        // -- CryptoActor's own five, real reply topics registering
        // on the same, shared router caused no real conflict at all.
        router
            .send_message(polar_router::RouterMessage::Route(
                polar_router::IncomingMessage {
                    topic: KUBERNETES_CONSUMER.to_string(),
                    payload: b"still works".to_vec(),
                    published_by: Some("kube-observer".to_string()),
                    trace_ctx: None,
                },
            ))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let got = received
            .lock()
            .unwrap()
            .clone()
            .expect("kube-consumer's own topic must still genuinely reach the supervisor");
        match got {
            ClientEvent::MessagePublished { topic, payload, .. } => {
                assert_eq!(topic, KUBERNETES_CONSUMER);
                assert_eq!(payload, b"still works");
            }
            other => panic!("expected MessagePublished, got {other:?}"),
        }
    }
}
