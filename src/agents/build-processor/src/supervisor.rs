use crate::project;
use cassini_types::{ClientEvent, WireTraceCtx};
use chrono::Utc;
use polar::{
    BUILD_EVENTS_TOPIC, BUILD_PROCESSOR_NAME, PROVENANCE_LINKER_TOPIC, ProvenanceEvent, RkyvError,
    SupervisorMessage,
    cassini::{CassiniClient, SubscribeRequest, TcpClient},
    get_neo_config,
    graph::controller::{GraphController, GraphControllerActor, GraphControllerMsg, GraphSignal},
    health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage},
};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

// ── State ──────────────────────────────────────────────────────────────────────

pub struct BuildProcessorState {
    pub broker_client: Arc<dyn CassiniClient>,
    /// Serializes every graph write. Spawned once the broker client registers;
    /// None until then, and events arriving earlier are dropped (the broker
    /// log is the source of truth).
    pub graph_controller: Option<GraphController>,
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
}

// ── Supervisor ─────────────────────────────────────────────────────────────────

/// Supervisor for the build processor: the single writer of build, artifact,
/// finding and registry facts into the graph.
///
/// Every agent publishes canonical [`ProvenanceEvent`]s to the broker. This
/// supervisor decodes each one, turns it into graph operations with
/// [`project::project`] (pure, see `project/`), and sends those operations in
/// order to the graph controller.
///
/// ## Actor tree
///
/// ```text
/// BuildProcessorSupervisor
///   ├── HealthCheckActor      (rejuvenation, dependency cert checks)
///   ├── TcpClient             (Cassini broker connection)
///   └── GraphControllerActor  (serializes every Neo4j write)
/// ```
///
/// There are no per-domain projector actors. Projection is stateless and every
/// write is an idempotent MERGE, so one mailbox costs nothing in throughput,
/// keeps each producer's events in order, and leaves exactly one dispatch.
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
/// An event that can't be projected is logged and dropped; it does not stop
/// the supervisor. A broker transport error is non-fatal: the client
/// reconnects and the healthcheck decides whether the outage warrants a
/// restart. The healthcheck stopping starts a drain window, then exits so the
/// orchestrator restarts the agent.
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

        // This supervisor owns a GraphControllerActor, so real
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

        // The graph controller loads its own connection config; loading it
        // here as well fails startup fast when the graph env is missing,
        // rather than at the first broker registration.
        get_neo_config()?;

        let broker_client = Arc::new(
            TcpClient::spawn(
                &format!("{BUILD_PROCESSOR_NAME}.tcp"),
                myself.clone(),
                |event| Some(SupervisorMessage::ClientEvent { event }),
            )
            .await?,
        );

        Ok(BuildProcessorState {
            broker_client,
            graph_controller: None,
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
                    info!("Cassini client registered — spawning graph controller and subscribing");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);

                    // ── Graph controller ───────────────────────────────────────
                    // Owns every Neo4j write this agent makes.
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

                    state.graph_controller = Some(controller);
                    let _ = state.healthcheck.cast(HealthCheckMessage::GraphConnected);

                    // ── Topic subscriptions ────────────────────────────────────
                    // BUILD_EVENTS_TOPIC: canonical ProvenanceEvent stream from
                    // all agents — CI pipeline, observer agents, resolver.
                    //
                    // PROVENANCE_LINKER_TOPIC: legacy topic. The GitLab consumer
                    // still publishes OCIRegistryDiscovered here; retained until
                    // it moves to BUILD_EVENTS_TOPIC.
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
                    let _ = state
                        .healthcheck
                        .cast(HealthCheckMessage::CassiniDisconnected);
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
                    let _ = state
                        .healthcheck
                        .cast(HealthCheckMessage::GraphOpFailed(reason));
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
                        .send_after(
                            ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS),
                            || SupervisorMessage::ForceExit,
                        )
                        .await;
                }
            }
            SupervisionEvent::ActorFailed(cell, e) => {
                let actor_name = cell.get_name().unwrap_or_default();
                error!("child actor failed: {:?} error: {:?}", actor_name, e);
                // TODO: implement selective restart — the graph controller
                // should restart on failure; broker client failure should stop the agent.
                if actor_name == HEALTHCHECK_ACTOR_NAME {
                    if !state.draining {
                        state.draining = true;
                        warn!("entering drain window before exit ({actor_name} failed)");
                        let _ = myself
                            .send_after(
                                ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS),
                                || SupervisorMessage::ForceExit,
                            )
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

    /// Project the event and send its graph operations, in order, to the
    /// graph controller. Routing lives in [`project::project`] alone.
    fn dispatch(event: ProvenanceEvent, state: &mut BuildProcessorState) {
        let Some(controller) = &state.graph_controller else {
            error!("graph controller not ready — dropping event");
            return;
        };
        let ops = match project::project(event, &Utc::now().to_rfc3339()) {
            Ok(ops) => ops,
            Err(e) => {
                warn!(error = %e, "event could not be projected — dropping it");
                return;
            }
        };
        for op in ops {
            if let Err(e) = controller.cast(GraphControllerMsg::Op(op)) {
                error!(error = %e, "failed to send graph operation to the controller");
                return;
            }
        }
    }
}
