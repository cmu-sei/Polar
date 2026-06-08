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
    graph::controller::{GraphController, GraphControllerActor},
};
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

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

                    // ── Graph controller ───────────────────────────────────────
                    // Owns Neo4j writes for execution lifecycle events.
                    let (controller, _) = Actor::spawn_linked(
                        Some(format!("{BUILD_PROCESSOR_NAME}.graph.controller")),
                        GraphControllerActor,
                        (),
                        myself.clone().into(),
                    )
                    .await?;

                    state.graph_controller = Some(controller.clone().into());

                    // ── Provenance linker ──────────────────────────────────────
                    // Owns Neo4j writes for artifact domain events.
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
                    Self::deserialize_and_dispatch(topic, payload, state);
                }

                ClientEvent::TransportError { reason } => {
                    error!("Transport error: {reason}");
                    myself.stop(Some(reason));
                }

                _ => {}
            },
        }

        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorStarted(cell) => {
                info!("child actor started: {:?}", cell.get_name());
            }
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                // Child termination is logged but does not stop the supervisor.
                // The graph can be rebuilt from the broker log.
                error!(
                    "child actor terminated: {:?} reason: {:?}",
                    cell.get_name(),
                    reason
                );
            }
            SupervisionEvent::ActorFailed(cell, e) => {
                error!("child actor failed: {:?} error: {:?}", cell.get_name(), e);
                // TODO: implement selective restart — graph controller and linker
                // should restart on failure; broker client failure should stop the agent.
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
            | ProvenanceEvent::ExecutionCancelled { .. }
            | ProvenanceEvent::VulnerabilityFound { .. } => {
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
            | ProvenanceEvent::OCIRegistryDiscovered { .. }
            | ProvenanceEvent::PodContainerUsesImage { .. } => {
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
            ProvenanceEvent::OCIArtifactDiscovered { uri } => {
                debug!("OCI artifact discovered: {uri} — resolver will handle");
            }
            ProvenanceEvent::ImageRefDiscovered { uri } => {
                debug!("image ref discovered: {uri} — resolver will handle");
            }
            ProvenanceEvent::ArtifactDiscovered { name, .. } => {
                debug!("artifact discovered: {name} — no handler yet");
            }
            ProvenanceEvent::BuildDiscovered { pipeline_id, .. } => {
                debug!("build discovered: {pipeline_id} — no handler yet");
            }

            ProvenanceEvent::Ignored => {}
        }
    }
}
