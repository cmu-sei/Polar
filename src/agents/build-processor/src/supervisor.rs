use crate::{
    build::{BuildActor, BuildActorArgs},
    linker::{ProvenanceLinker, ProvenanceLinkerArgs},
};
use cassini_types::{ClientEvent, WireTraceCtx};
use neo4rs::Graph;
use polar::{
    BUILD_EVENTS_TOPIC, BUILD_PROCESSOR_NAME, PROVENANCE_LINKER_TOPIC, ProvenanceEvent, RkyvError,
    SupervisorMessage,
    cassini::{CassiniClient, SubscribeRequest, TcpClient},
    get_neo_config,
    graph::controller::{GraphController, GraphControllerActor, GraphSignal},
    health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage},
};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

// ── State ──────────────────────────────────────────────────────────────────────

pub struct BuildProcessorState {
    pub graph: Graph,
    pub broker_client: Arc<dyn CassiniClient>,
    /// Graph controller for execution lifecycle projection (BuildJob, BuildStage, etc.)
    pub graph_controller: Option<GraphController>,
    /// Linker actor for artifact domain projection (Sbom, Package, ContainerImage, etc.)
    pub linker: Option<ActorRef<ProvenanceEvent>>,
    /// Linker actor for artifact domain projection (Sbom, Package, ContainerImage, etc.)
    pub build_handler: Option<ActorRef<ProvenanceEvent>>,
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
}

// ── Supervisor ─────────────────────────────────────────────────────────────────

/// Supervisor for the unified build processor agent.
///
/// Owns the full pipeline event projection — both build execution lifecycle
/// and artifact provenance — in a single subscription loop. All CI systems
/// and observer agents publish canonical [`ProvenanceEvent`] instances to
/// the broker; this supervisor deserializes and dispatches them.
///
/// ## Actor tree
///
/// ```
/// BuildProcessorSupervisor
///   ├── TcpClient          (Cassini broker connection)
///   ├── GraphControllerActor (Neo4j writes for execution lifecycle events)
///   └── ProvenanceLinker   (Neo4j writes for artifact domain events)
/// ```
///
/// ## Deserialization
///
/// Two wire formats are accepted for [`ProvenanceEvent`]:
/// - rkyv: Rust agents (k8s observer, GitLab observer, resolver)
/// - JSON: nushell pipeline stages (core.nu emit-* functions)
///
/// rkyv is attempted first as the hot path. JSON fallback handles the
/// pipeline. If both fail the message is dropped with a warning — the
/// broker log is the source of truth and replaying will recover lost events.
///
/// ## Fault tolerance
///
/// Projection failures on individual events do not stop the supervisor.
/// Child actor failures restart the affected child rather than stopping
/// the supervisor, except for the broker client — a transport error is
/// unrecoverable and stops the agent.
pub struct BuildProcessorSupervisor;

#[async_trait]
impl Actor for BuildProcessorSupervisor {
    type Msg = SupervisorMessage;
    type State = BuildProcessorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("BuildProcessorSupervisor starting");

        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        // This supervisor owns a GraphControllerActor directly (via
        // BuildActor/ProvenanceLinker, both writing through it), so real
        // GraphAvailable/GraphOpFailed tracking applies here.
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

        let graph = neo4rs::Graph::connect(get_neo_config()?)?;

        let broker_client = Arc::new(
            TcpClient::spawn(
                &format!("{BUILD_PROCESSOR_NAME}.tcp"),
                myself.clone(),
                |event| Some(SupervisorMessage::ClientEvent { event }),
            )
            .await?,
        );

        Ok(BuildProcessorState {
            graph,
            broker_client,
            graph_controller: None,
            linker: None,
            build_handler: None,
            healthcheck,
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
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    info!("Cassini client registered — spawning child actors and subscribing");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);

                    // ── Graph controller ───────────────────────────────────────
                    // Owns Neo4j writes for execution lifecycle events.
                    // BUG FIX: signal_port was previously a throwaway
                    // OutputPort with nothing subscribed to it — every
                    // GraphSignal fired by the controller was silently
                    // discarded. Now routed into SupervisorMessage::GraphSignal.
                    let graph_signal_port = Arc::new(OutputPort::<GraphSignal>::default());
                    graph_signal_port.subscribe(myself.clone(), |signal| {
                        Some(SupervisorMessage::GraphSignal(signal))
                    });
                    let (controller, _) = Actor::spawn_linked(
                        Some(format!("{BUILD_PROCESSOR_NAME}.graph.controller")),
                        GraphControllerActor,
                        polar::graph::controller::GraphControllerArgs {
                            signal_port: graph_signal_port,
                        },
                        myself.clone().into(),
                    )
                    .await?;

                    state.graph_controller = Some(controller.clone().into());
                    let _ = state.healthcheck.cast(HealthCheckMessage::GraphConnected);

                    // ── Build handler ───────────────────────────────────────────
                    let (build_handler, _) = Actor::spawn_linked(
                        Some(format!("{BUILD_PROCESSOR_NAME}.build_handler")),
                        BuildActor,
                        BuildActorArgs {
                            graph_controller: controller.clone(),
                        },
                        myself.clone().into(),
                    )
                    .await?;

                    state.build_handler = Some(build_handler);

                    // ── Provenance linker ──────────────────────────────────────
                    // Owns Neo4j writes for artifact domain events.
                    let (linker, _) = Actor::spawn_linked(
                        Some(format!("{BUILD_PROCESSOR_NAME}.linker")),
                        ProvenanceLinker,
                        ProvenanceLinkerArgs {
                            compiler: controller,
                        },
                        myself.clone().into(),
                    )
                    .await?;

                    state.linker = Some(linker);

                    // ── Topic subscriptions ────────────────────────────────────
                    // BUILD_EVENTS_TOPIC: canonical ProvenanceEvent stream from
                    // all agents — CI pipeline, observer agents, resolver.
                    //
                    // PROVENANCE_LINKER_TOPIC: legacy topic still used by k8s
                    // and GitLab agents for OCI discovery events. Retained until
                    // those agents migrate to BUILD_EVENTS_TOPIC.
                    for topic in [BUILD_EVENTS_TOPIC, PROVENANCE_LINKER_TOPIC] {
                        if let Err(e) = state.broker_client.subscribe(SubscribeRequest {
                            topic: topic.to_string(),
                            trace_ctx: WireTraceCtx::from_current_span(),
                        }) {
                            error!("Failed to subscribe to topic {topic}: {e}");
                            return Err(e.into());
                        }
                        debug!("Subscribed to {topic}");
                    }
                }

                ClientEvent::MessagePublished { topic, payload, .. } => {
                    if state.draining {
                        warn!(
                            "draining -- logging message on topic '{topic}' \
                             ({} bytes) instead of dispatching; will be lost on exit",
                            payload.len()
                        );
                    } else {
                        Self::deserialize_and_dispatch(topic, payload, state);
                    }
                }

                // BUG FIX: was `myself.stop(Some(reason))` -- silent exit-0,
                // Job marks Completed, no restart, no visible failure.
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniDisconnected);
                }

                _ => {}
            },
            SupervisorMessage::Heartbeat => {}
            SupervisorMessage::PrepareShutdown => {
                info!("PrepareShutdown received");
                if let Err(e) = state.healthcheck.cast(HealthCheckMessage::ShutdownAck) {
                    error!("failed to send ShutdownAck: {e}");
                }
            }
            SupervisorMessage::GraphSignal(signal) => match signal {
                GraphSignal::Available => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::GraphAvailable);
                }
                GraphSignal::OpFailed(reason) => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::GraphOpFailed(reason));
                }
            },
            SupervisorMessage::ForceExit => {
                warn!("drain window elapsed, exiting now");
                std::process::exit(1);
            }
        }

        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorStarted(cell) => {
                info!("child actor started: {:?}", cell.get_name());
            }
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                let actor_name = cell.get_name().unwrap_or_default();
                // Child termination is logged but does not stop the supervisor.
                // The graph can be rebuilt from the broker log.
                error!(
                    "child actor terminated: {:?} reason: {:?}",
                    actor_name, reason
                );
                if actor_name == HEALTHCHECK_ACTOR_NAME && !state.draining {
                    state.draining = true;
                    info!("entering drain window before exiting for rejuvenation");
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            SupervisionEvent::ActorFailed(cell, e) => {
                let actor_name = cell.get_name().unwrap_or_default();
                error!("child actor failed: {:?} error: {:?}", actor_name, e);
                // TODO: implement selective restart — graph controller and linker
                // should restart on failure; broker client failure should stop the agent.
                if actor_name == HEALTHCHECK_ACTOR_NAME {
                    if !state.draining {
                        state.draining = true;
                        warn!("entering drain window before exit ({actor_name} failed)");
                        let _ = myself
                            .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                                SupervisorMessage::ForceExit
                            })
                            .await;
                    }
                }
            }
            SupervisionEvent::ProcessGroupChanged(..) => {}
        }
        Ok(())
    }
}

// ── Deserialization ────────────────────────────────────────────────────────────

impl BuildProcessorSupervisor {
    /// Deserialize a raw broker payload into a [`ProvenanceEvent`] and dispatch
    /// to the appropriate handler.
    ///
    /// rkyv is attempted first (Rust agent hot path). JSON fallback handles
    /// nushell pipeline emissions. Both paths produce the same `ProvenanceEvent`
    /// type — no intermediary translation layer.
    #[instrument(
        name = "BuildProcessorSupervisor::deserialize_and_dispatch",
        skip(payload, state)
    )]
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>, state: &mut BuildProcessorState) {
        debug!("received message on topic {topic}");

        let event = if let Ok(e) = rkyv::from_bytes::<ProvenanceEvent, RkyvError>(&payload) {
            e
        } else if let Ok(e) = serde_json::from_slice::<ProvenanceEvent>(&payload) {
            e
        } else {
            let rkyv_err = rkyv::from_bytes::<ProvenanceEvent, RkyvError>(&payload).unwrap_err();
            let json_err = serde_json::from_slice::<ProvenanceEvent>(&payload).unwrap_err();
            warn!(
                topic = %topic,
                rkyv_error = %rkyv_err,
                json_error = %json_err,
                "failed to deserialize ProvenanceEvent — dropping message"
            );
            return;
        };

        Self::dispatch(event, state);
    }

    /// Route a deserialized [`ProvenanceEvent`] to the correct handler.
    ///
    /// Execution lifecycle events are projected inline via `project_event` —
    /// they need the graph controller and the build_id from the event itself.
    ///
    /// Artifact domain events are forwarded to the linker actor, which owns
    /// the artifact graph projection logic.
    fn dispatch(event: ProvenanceEvent, state: &mut BuildProcessorState) {
        match &event {
            // ── Execution lifecycle ────────────────────────────────────────────
            // Projected inline — build_id is on the variant, graph controller
            // is in state. No actor hop needed.
            ProvenanceEvent::ExecutionStarted { .. }
            | ProvenanceEvent::StageStarted { .. }
            | ProvenanceEvent::StageCompleted { .. }
            | ProvenanceEvent::ExecutionCompleted { .. }
            | ProvenanceEvent::ExecutionFailed { .. }
            | ProvenanceEvent::ExecutionCancelled { .. } => {
                let Some(handler) = &state.build_handler else {
                    error!("build handler not ready — dropping lifecycle event");
                    return;
                };
                if let Err(e) = handler.send_message(event) {
                    error!(error = %e, "failed to forward event to linker");
                }
            }

            // ── Artifact domain ────────────────────────────────────────────────
            // Forwarded to the linker actor which owns these handlers.
            ProvenanceEvent::ArtifactProduced(_)
            | ProvenanceEvent::SbomAnalyzed(_)
            | ProvenanceEvent::BinaryLinked(_)
            | ProvenanceEvent::ContainerImageCreated(_)
            | ProvenanceEvent::OCIArtifactResolved { .. }
            | ProvenanceEvent::OCIArtifactCreated { .. }
            | ProvenanceEvent::ImageRefResolved { .. }
            | ProvenanceEvent::SecurityAdvisoryFound { .. }
            | ProvenanceEvent::PackageStatusFound { .. }
            | ProvenanceEvent::OCIRegistryDiscovered { .. } => {
                let Some(linker) = &state.linker else {
                    error!("linker not ready — dropping artifact event");
                    return;
                };
                if let Err(e) = linker.send_message(event) {
                    error!(error = %e, "failed to forward event to linker");
                }
            }

            // ── Discovery events ───────────────────────────────────────────────
            // These trigger the resolver — log and discard here, the resolver
            // subscribes to PROVENANCE_DISCOVERY_TOPIC independently.
            ProvenanceEvent::OCIArtifactDiscovered { uri, .. } => {
                debug!("OCI artifact discovered: {uri} — resolver will handle");
            }
            ProvenanceEvent::ArtifactDiscovered { name, .. } => {
                debug!("artifact discovered: {name} — no handler yet");
            }

            ProvenanceEvent::Ignored => {}
        }
    }
}
