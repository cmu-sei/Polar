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
use polar::DiscoverySourceRef;
use polar::ProvenanceEvent;
use polar::RkyvError;
use polar::SupervisorMessage;
use polar::cassini::CassiniClient;
use polar::cassini::TcpClient;
use polar::graph::controller::{
    GraphController, GraphControllerActor, GraphControllerArgs, GraphControllerMsg, GraphOp,
    GraphSignal, IntoGraphKey,
};
use polar::graph::nodes::builds::ArtifactNodeKey;
use polar::graph::nodes::kube::KubeNodeKey;
use polar::health::DepCertEndpoint;
use polar::topics::KUBERNETES_RESOLUTION_EVENTS;
use polar::health::HealthCheckArgs;
use polar_agent_builder::{AgentBuilder, CryptoHandle, HealthClient, HealthHandle, Subscription};
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
    healthcheck: HealthHandle,
    /// The real, first, genuine CryptoActor user -- shares this
    /// supervisor's own, already-established Cassini connection and
    /// RouterActor rather than opening a second connection. Not yet
    /// consumed by anything in this file's own, real projection
    /// logic; the real, upcoming, plaintext-Kubernetes-secret
    /// encryption work (see this crate's own, real design
    /// discussion) is the actual, first real caller.
    crypto: CryptoHandle,
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

    /// Builds this agent's own two, real, genuinely-owned
    /// Subscriptions (KUBERNETES_CONSUMER, KUBERNETES_RESOLUTION_EVENTS)
    /// -- each one's own, real deliver closure reconstructs the exact
    /// same ClientEvent::MessagePublished this agent's own, existing,
    /// unchanged handle()/deserialize_and_dispatch logic already
    /// expects, and forwards it to `supervisor` as the same, existing
    /// SupervisorMessage::ClientEvent it already handles.
    ///
    /// Extracted into its own, real, standalone function -- not left
    /// inlined inside pre_start -- specifically so a real test can
    /// exercise these two, real, exact closures directly, without
    /// needing the real network connection pre_start itself,
    /// separately, attempts. Mirrors this file's own, previous,
    /// established register_kube_topics's own, real testability
    /// design exactly; only how the results get registered with
    /// RouterActor changed (AgentBuilder's own, real
    /// .with_subscription(...) now owns that part).
    fn kube_subscriptions(supervisor: &ActorRef<SupervisorMessage>) -> Vec<Subscription> {
        let supervisor_for_consumer = supervisor.clone();
        let supervisor_for_resolution = supervisor.clone();
        vec![
            Subscription::new(KUBERNETES_CONSUMER, move |incoming| {
                let event = ClientEvent::MessagePublished {
                    topic: incoming.topic,
                    payload: incoming.payload,
                    trace_ctx: incoming.trace_ctx,
                    published_by: incoming.published_by,
                };
                if let Err(e) =
                    supervisor_for_consumer.send_message(SupervisorMessage::ClientEvent { event })
                {
                    warn!("AgentBuilder: failed to deliver routed message to supervisor: {e}");
                }
            }),
            Subscription::new(KUBERNETES_RESOLUTION_EVENTS, move |incoming| {
                let event = ClientEvent::MessagePublished {
                    topic: incoming.topic,
                    payload: incoming.payload,
                    trace_ctx: incoming.trace_ctx,
                    published_by: incoming.published_by,
                };
                if let Err(e) =
                    supervisor_for_resolution.send_message(SupervisorMessage::ClientEvent { event })
                {
                    warn!("AgentBuilder: failed to deliver routed message to supervisor: {e}");
                }
            }),
        ]
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

        // Parse dep cert endpoints for this agent:
        // kube-consumer depends on Cassini and Neo4j.
        let dep_cert_endpoints = vec![
            DepCertEndpoint::parse("cassini-ip-svc.polar.svc.cluster.local:8080:300")
                .map_err(ActorProcessingErr::from)?,
            DepCertEndpoint::parse("polar-db-svc.polar-graph.svc.cluster.local:7687:300")
                .map_err(ActorProcessingErr::from)?,
        ];

        // The real, entire, formerly hand-rolled sequence --
        // HealthCheckActor, RouterActor, this agent's own two,
        // genuinely-owned topics (each bound to the real, wire-level
        // Subscribe AgentBuilder itself now, automatically fires on
        // ClientEvent::Registered -- see polar-agent-builder's own
        // Subscription for the complete, real reasoning), TcpClient,
        // and CryptoActor -- collapsed into one, real, composed call.
        let mut builder = AgentBuilder::new(myself.clone(), BROKER_CLIENT_NAME)
            .with_health_check(HealthCheckArgs {
                expects_graph: true,
                rejuvenation_threshold_secs: 300,
                dep_cert_endpoints,
            });
        for subscription in Self::kube_subscriptions(&myself) {
            builder = builder.with_subscription(subscription);
        }
        let resources = builder
            .with_lifecycle_handler(|event| Some(SupervisorMessage::ClientEvent { event }))
            .with_crypto()
            .build()
            .await?;

        // Eager graph controller spawn -- this agent's own health
        // never genuinely needs to wait on Cassini registration
        // first; a real Neo4j connection is entirely independent of
        // it (the same, real reasoning credentials-agent's own
        // supervisor already established). GraphControllerActor::pre_start
        // only builds real pool config; it doesn't handshake, so
        // HealthCheckMessage::GraphAvailable (not GraphConnected
        // alone) is still what genuinely proves real connectivity.
        let graph_signal_port = Arc::new(OutputPort::<GraphSignal>::default());
        graph_signal_port.subscribe(myself.clone(), |signal| {
            Some(SupervisorMessage::GraphSignal(signal))
        });
        let (graph_controller, _) = GraphControllerActor::spawn_linked(
            Some("kubernetes.cluster.graph.controller".to_string()),
            GraphControllerActor,
            GraphControllerArgs {
                signal_port: graph_signal_port,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        Ok(ClusterConsumerSupervisorState {
            broker_client: resources.tcp_client,
            graph_controller: Some(graph_controller),
            projection_cache: ProjectionCache {
                entries: HashMap::new(),
            },
            healthcheck: resources
                .healthcheck
                .expect("with_health_check was called, so this is always real, genuinely Some"),
            crypto: resources
                .crypto
                .expect("with_crypto was called, so this is always real, genuinely Some"),
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
            SupervisorMessage::GraphSignal(signal) => match signal {
                GraphSignal::Available => {
                    let _ = state.healthcheck.graph_available();
                }
                GraphSignal::OpFailed(reason) => {
                    let _ = state.healthcheck.graph_op_failed(reason);
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
                        // AgentBuilder's own, internal lifecycle
                        // wiring already, automatically fired the
                        // real, wire-level Subscribe for both of
                        // this agent's own topics the moment this
                        // event genuinely fired -- see Subscription's
                        // own, real doc comment. The graph controller
                        // is spawned eagerly in pre_start now, not
                        // here, so there is no real, conditional
                        // spawn-on-Registered logic left at all --
                        // only these two, real, simple health
                        // signals remain.
                        let _ = state.healthcheck.cassini_connected();
                        let _ = state.healthcheck.graph_connected();
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
                        let _ = state.healthcheck.cassini_disconnected();
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
                //
                // Matched by real, genuine actor id, not name --
                // AgentBuilder spawns HealthCheckActor unnamed (see
                // its own, real with_health_check implementation), so
                // a real, direct id comparison against this
                // supervisor's own, held HealthHandle is both the
                // only real option available and, genuinely, more
                // robust than name-matching ever was (ids never
                // collide across different, real actor types; names,
                // in principle, could).
                if actor_cell.get_id() == state.healthcheck.as_actor_ref().get_id() && !state.draining {
                    state.draining = true;
                    info!(
                        "CONSUMER_SUPERVISOR: healthcheck actor terminated cleanly, \
                         entering drain window before exiting for rejuvenation, \
                         unsubscribing from Cassini"
                    );
                    // Genuinely, previously part of a now-dead
                    // SupervisorMessage::PrepareShutdown handler --
                    // nothing sends that message here anymore, but
                    // this real, graceful step (unsubscribe before
                    // going dark) still, genuinely matters, so it
                    // moves here, to the actual, real place
                    // rejuvenation is now detected.
                    if let Err(e) = state.broker_client.unsubscribe(polar::cassini::UnsubscribeRequest {
                        topic: KUBERNETES_CONSUMER.to_string(),
                        trace_ctx: None,
                    }) {
                        warn!("CONSUMER_SUPERVISOR: unsubscribe failed (continuing): {e}");
                    }
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
mod subscription_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A minimal, real stand-in for ClusterConsumerSupervisor itself
    /// -- genuinely receives SupervisorMessage, just records what it
    /// got, since this test's own, real purpose is proving
    /// kube_subscriptions's own, two, real deliver closures
    /// correctly reconstruct and forward ClientEvent::MessagePublished,
    /// not re-testing this crate's own, separate, unchanged
    /// handle()/deserialize_and_dispatch logic.
    struct FakeSupervisor {
        received: Arc<Mutex<Vec<ClientEvent>>>,
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
                self.received.lock().unwrap().push(event);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn kube_subscriptions_returns_exactly_this_agents_own_two_real_topics() {
        let (fake_supervisor, _) = Actor::spawn(
            None,
            FakeSupervisor {
                received: Arc::new(Mutex::new(Vec::new())),
            },
            (),
        )
        .await
        .unwrap();

        let subscriptions = ClusterConsumerSupervisor::kube_subscriptions(&fake_supervisor);

        assert_eq!(subscriptions.len(), 2);
        let topics: Vec<&str> = subscriptions.iter().map(|s| s.topic.as_str()).collect();
        assert!(topics.contains(&KUBERNETES_CONSUMER));
        assert!(topics.contains(&KUBERNETES_RESOLUTION_EVENTS));
    }

    #[tokio::test]
    async fn each_real_deliver_closure_genuinely_forwards_a_correctly_reconstructed_client_event() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (fake_supervisor, _) = Actor::spawn(
            None,
            FakeSupervisor {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let subscriptions = ClusterConsumerSupervisor::kube_subscriptions(&fake_supervisor);

        // The real, direct proof: invoke each of the two, real
        // deliver closures directly -- no RouterActor involved at
        // all, since this function no longer registers with one
        // itself (AgentBuilder's own, real .with_subscription(...)
        // does that now, separately, and is already, separately
        // tested in polar-agent-builder's own test suite).
        for subscription in &subscriptions {
            (subscription.deliver)(polar_router::IncomingMessage {
                topic: subscription.topic.clone(),
                payload: format!("payload for {}", subscription.topic).into_bytes(),
                published_by: Some("kube-observer".to_string()),
                trace_ctx: None,
            });
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let events = received.lock().unwrap();
        assert_eq!(events.len(), 2);
        for event in events.iter() {
            match event {
                ClientEvent::MessagePublished {
                    topic,
                    payload,
                    published_by,
                    ..
                } => {
                    assert_eq!(payload, format!("payload for {topic}").as_bytes());
                    assert_eq!(published_by, &Some("kube-observer".to_string()));
                }
                other => panic!("expected MessagePublished, got {other:?}"),
            }
        }
    }
}
