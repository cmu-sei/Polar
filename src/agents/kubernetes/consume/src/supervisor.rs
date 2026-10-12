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
use polar::cassini::CassiniClient;
use polar::cassini::TcpClient;
use polar::graph::controller::{
    GraphController, GraphControllerActor, GraphControllerArgs, GraphControllerMsg, GraphOp,
    GraphSignal, IntoGraphKey,
};
use polar::graph::nodes::builds::ArtifactNodeKey;
use polar::graph::nodes::kube::KubeNodeKey;
use polar::topics::KUBERNETES_RESOLUTION_EVENTS;
use polar_agent_builder::{
    AgentBuilder, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage, Subscription,
};
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use ractor::async_trait;
use ractor::concurrency::Duration;
use ractor::{Actor, OutputPort};
use serde::de::DeserializeOwned;
use serde_json::from_value;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

const DRAIN_WINDOW_SECS: u64 = 5;

/// How long the infrastructure supervisor gives this agent to drain: the drain
/// window and the graph controller's stop (up to 30 s), plus a margin. If it is
/// not done by then the process exits 1 regardless.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(DRAIN_WINDOW_SECS + 30 + 10);

/// This supervisor's own message type. The shared `polar::SupervisorMessage`
/// cannot carry the two infrastructure signals (respawn, prepare-shutdown)
/// without `polar-agent-builder` depending back on `polar`, so, like
/// kube-observer and jira-processor, the consumer defines exactly the
/// messages it handles.
pub enum ConsumerSupervisorMessage {
    ClientEvent {
        event: ClientEvent,
    },
    GraphSignal(GraphSignal),
    /// HealthCheckActor, a child of the InfrastructureSupervisor, terminated
    /// cleanly for rejuvenation. The infrastructure now waits for this
    /// supervisor to finish its own shutdown and answer with
    /// `InfrastructureSupervisorMessage::ProceedWithShutdown`.
    PrepareShutdown,
    /// The drain window has elapsed. After a hard failure this exits the
    /// process; after `PrepareShutdown` it flushes the graph controller and
    /// completes the handshake.
    DrainElapsed,
}

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
    /// The InfrastructureSupervisor (HealthCheckActor, RouterActor,
    /// TcpClientActor, CryptoActor) this supervisor's builder spawned. Its
    /// termination means the shutdown handshake is complete (or that
    /// infrastructure died), and either way this process must exit.
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    /// Set true once a drain window has begun, either after a fatal failure
    /// or for rejuvenation. While true, incoming messages are logged instead
    /// of processed, since whatever failed (likely the graph controller)
    /// can't be trusted, and we're exiting shortly regardless.
    draining: bool,
    /// Set true when the drain was started by `PrepareShutdown`, so the end
    /// of the drain completes the handshake with the infrastructure instead
    /// of exiting directly.
    rejuvenating: bool,
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
    /// ConsumerSupervisorMessage::ClientEvent it already handles.
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
    fn kube_subscriptions(supervisor: &ActorRef<ConsumerSupervisorMessage>) -> Vec<Subscription> {
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
                if let Err(e) = supervisor_for_consumer
                    .send_message(ConsumerSupervisorMessage::ClientEvent { event })
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
                if let Err(e) = supervisor_for_resolution
                    .send_message(ConsumerSupervisorMessage::ClientEvent { event })
                {
                    warn!("AgentBuilder: failed to deliver routed message to supervisor: {e}");
                }
            }),
        ]
    }

    /// Schedules `DrainElapsed` after the drain window. Not awaited: the
    /// timer runs detached, so this supervisor keeps handling (and logging)
    /// messages while the window is open.
    fn schedule_drain_end(myself: &ActorRef<ConsumerSupervisorMessage>) {
        let _ = myself.send_after(Duration::from_secs(DRAIN_WINDOW_SECS), || {
            ConsumerSupervisorMessage::DrainElapsed
        });
    }

    /// Tells the broker this agent is going dark, so it stops routing
    /// messages here. Failures are logged and do not stop the shutdown.
    fn unsubscribe_from_topics(client: &TcpClient) {
        for topic in [KUBERNETES_CONSUMER, KUBERNETES_RESOLUTION_EVENTS] {
            if let Err(e) = client.unsubscribe(polar::cassini::UnsubscribeRequest {
                topic: topic.to_string(),
                trace_ctx: None,
            }) {
                warn!("CONSUMER_SUPERVISOR: unsubscribe from '{topic}' failed (continuing): {e}");
            }
        }
    }
}

#[async_trait]
impl Actor for ClusterConsumerSupervisor {
    type Msg = ConsumerSupervisorMessage;
    type State = ClusterConsumerSupervisorState;
    /// The configuration `run` judged: the builder is made from it, so what the agent declared
    /// (its capabilities, its budget, the self-test) is what the infrastructure is built to.
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        info!("Read neo configuration successfully.");

        // The real, entire, formerly hand-rolled sequence --
        // HealthCheckActor, RouterActor, this agent's own two,
        // genuinely-owned topics (each bound to the real, wire-level
        // Subscribe AgentBuilder itself now, automatically fires on
        // ClientEvent::Registered -- see polar-agent-builder's own
        // Subscription for the complete, real reasoning), TcpClient,
        // and CryptoActor -- collapsed into one, real, composed call.
        let mut builder = AgentBuilder::new(myself.clone(), BROKER_CLIENT_NAME);

        for subscription in Self::kube_subscriptions(&myself) {
            builder = builder.with_subscription(subscription);
        }
        let resources = builder
            .with_lifecycle_handler(|event| Some(ConsumerSupervisorMessage::ClientEvent { event }))
            .into_configured(configured)
            .build()
            .await?;

        // The "infrastructure -> domain" half of the respawn/shutdown
        // protocol. Without this subscription the InfrastructureSupervisor
        // would wait forever for a ProceedWithShutdown after rejuvenation.
        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => {
                    Some(ConsumerSupervisorMessage::PrepareShutdown)
                }
            });

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
            Some(ConsumerSupervisorMessage::GraphSignal(signal))
        });
        let (graph_controller, _) = GraphControllerActor::spawn_linked(
            Some("kubernetes.cluster.graph.controller".to_string()),
            GraphControllerActor,
            // The settings come from the builder's snapshot, which already judged them.
            GraphControllerArgs::new(graph_signal_port).with_settings(
                polar::graph::environment::GraphSettings::from_env(&*resources.config)
                    .map_err(ActorProcessingErr::from)?,
            ),
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        // The encryption capability must have produced the crypto actor; nothing here uses it.
        resources.crypto.as_ref().ok_or_else(|| {
            ActorProcessingErr::from(
                "with_encryption() was called but the builder returned no CryptoHandle",
            )
        })?;

        Ok(ClusterConsumerSupervisorState {
            broker_client: resources.tcp_client,
            graph_controller: Some(graph_controller),
            projection_cache: ProjectionCache {
                entries: HashMap::new(),
            },
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            draining: false,
            rejuvenating: false,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ConsumerSupervisorMessage::GraphSignal(signal) => match signal {
                GraphSignal::Available => {
                    let _ = state.healthcheck.graph_available();
                }
                GraphSignal::OpFailed(reason) => {
                    let _ = state.healthcheck.graph_op_failed(reason);
                }
            },

            ConsumerSupervisorMessage::PrepareShutdown => {
                if state.draining {
                    // A hard failure already started a drain that ends in
                    // exit(1); the handshake is moot.
                    warn!(
                        "CONSUMER_SUPERVISOR: PrepareShutdown received while already draining; \
                         the pending exit will end the process"
                    );
                } else {
                    state.draining = true;
                    state.rejuvenating = true;
                    info!(
                        "CONSUMER_SUPERVISOR: PrepareShutdown received (rejuvenation), \
                         unsubscribing from Cassini and entering drain window"
                    );
                    Self::unsubscribe_from_topics(&state.broker_client);
                    Self::schedule_drain_end(&myself);
                }
            }

            ConsumerSupervisorMessage::DrainElapsed => {
                warn!("CONSUMER_SUPERVISOR: drain window elapsed, beginning shutdown");

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

                if state.rejuvenating {
                    // Our own children are down; let the infrastructure stop.
                    // Its termination (handled in handle_supervisor_evt)
                    // exits the process with a non-zero status.
                    info!(
                        "CONSUMER_SUPERVISOR: domain shutdown complete, releasing infrastructure"
                    );
                    if let Err(e) = state
                        .infrastructure
                        .cast(InfrastructureSupervisorMessage::ProceedWithShutdown)
                    {
                        error!(
                            "CONSUMER_SUPERVISOR: could not tell infrastructure to proceed ({e}); \
                             exiting now"
                        );
                        std::process::exit(1);
                    }
                } else {
                    warn!("CONSUMER_SUPERVISOR: exiting now");
                    std::process::exit(1);
                }
            }

            ConsumerSupervisorMessage::ClientEvent { event } => {
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
                // The InfrastructureSupervisor is this supervisor's direct
                // child (the builder spawned it linked); the health check
                // is its child, not ours, so rejuvenation reaches us as
                // PrepareShutdown, not as a termination event. Infrastructure
                // terminating is the end of the handshake -- or infrastructure
                // dying on its own -- and in both cases the process exits so
                // the Job starts a new pod with fresh certs. Matched by id,
                // since the actor is spawned unnamed.
                if actor_cell.get_id() == state.infrastructure.get_id() {
                    info!(
                        "CONSUMER_SUPERVISOR: infrastructure terminated, exiting so the Job restarts the pod"
                    );
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
                // Any child actor failure is fatal, but don't exit
                // immediately -- give the mailbox loop a bounded window to
                // keep logging any in-flight messages (handle() branches on
                // state.draining above) before DrainElapsed actually exits.
                if !state.draining {
                    state.draining = true;
                    warn!(
                        "CONSUMER_SUPERVISOR: entering drain window before exit; \
                         any messages that arrive will be logged, not processed"
                    );
                    Self::schedule_drain_end(&myself);
                }
            }
            SupervisionEvent::ProcessGroupChanged(..) => {}
        }

        Ok(())
    }
}

#[cfg(test)]
mod subscription_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A minimal, real stand-in for ClusterConsumerSupervisor itself
    /// -- genuinely receives ConsumerSupervisorMessage, just records what it
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
        type Msg = ConsumerSupervisorMessage;
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
            if let ConsumerSupervisorMessage::ClientEvent { event } = message {
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
