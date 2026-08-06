use cassini_types::ClientEvent;
use orchestrator_core::{
    events::{BuildEvent, EventPayload},
    types::subjects::BUILD_EVENTS_TOPIC,
};
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use polar::{
    RkyvError, SupervisorMessage,
    cassini::{CassiniClient, SubscribeRequest},
    get_neo_config,
    graph::{
        controller::{
            GraphController, GraphControllerActor, GraphControllerMsg, GraphOp, GraphSignal,
            GraphValue, IntoGraphKey, Property, rel::BUILT_BY,
        },
        nodes::{builds::BuildNodeKey, git::GitNodeKey},
    },
};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};
use rkyv::from_bytes;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

// ── Event projection ───────────────────────────────────────────────────────────

/// Project a `BuildEvent` into graph operations.
///
/// Each event type maps to a specific set of graph mutations. The processor
/// is intentionally stateless — it does not cache or aggregate events. Each
/// event is projected independently, which means operations are idempotent:
/// replaying events from the Cassini log produces the same graph state.
///
/// Node ownership rules enforced here:
/// - BuildJob nodes: owned by Cyclops, freely upserted.
/// - GitCommit, Image nodes: foreign, only referenced via EnsureEdge.
///   We do not call UpsertNode on these — the authoritative agent owns them.
pub fn project_event(
    event: &BuildEvent,
    graph: &GraphController,
) -> Result<(), ActorProcessingErr> {
    let build_id = event.build_id.to_string();
    // 1. Create a DateTime<Utc> from the i64 seconds
    // from_timestamp_secs returns an Option<Self>, so we use unwrap() for simplicity
    if let Some(d) = chrono::DateTime::from_timestamp_secs(event.emitted_at) {
        let now = d.to_rfc3339();

        match &event.payload {
            EventPayload::BuildStarted {
                repo_url,
                commit_sha,
                requested_by,
                job_identity,
            } => {
                // ── Upsert the BuildJob anchor node ────────────────────────────────
                // This is the first event in the lifecycle — create the node.
                // Subsequent events update its state via TRANSITIONED_TO edges.
                let job_key = BuildNodeKey::BuildJob {
                    build_id: build_id.clone(),
                };

                graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: job_key.clone().into_key(),
                    props: vec![
                        Property("build_id".into(), GraphValue::String(build_id.clone())),
                        Property("repo_url".into(), GraphValue::String(repo_url.clone())),
                        Property("commit_sha".into(), GraphValue::String(commit_sha.clone())),
                        Property(
                            "requested_by".into(),
                            GraphValue::String(requested_by.clone()),
                        ),
                        Property("started_at".into(), GraphValue::String(now.clone())),
                        Property("observed_at".into(), GraphValue::String(now.clone())),
                    ],
                }))?;

                // ── Initial state node ─────────────────────────────────────────────
                let state_key = BuildNodeKey::BuildJobState {
                    build_id: build_id.clone(),
                    valid_from: now.clone(),
                };

                graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                    resource_key: job_key.clone().into_key(),
                    state_type_key: BuildNodeKey::State.into_key(),
                    state_instance_key: state_key.into_key(),
                    state_instance_props: vec![
                        Property("phase".into(), GraphValue::String("scheduled".into())),
                        Property("valid_from".into(), GraphValue::String(now.clone())),
                    ],
                }))?;

                // ── Edge: GitCommit -[:BUILT_BY]-> BuildJob ────────────────────────
                // The GitCommit node is owned by the VCS agent. We do not upsert it —
                // if it doesn't exist yet the edge will be created when it appears.
                // EnsureEdge uses MERGE semantics so this is safe to call speculatively.
                graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                    from: GitNodeKey::Commit {
                        oid: commit_sha.clone(),
                    }
                    .into_key(),
                    rel_type: BUILT_BY.to_string(),
                    to: job_key.into_key(),
                    props: vec![Property("at".into(), GraphValue::String(now.clone()))],
                }))?;

                graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                    from: BuildNodeKey::BuildJob {
                        build_id: build_id.clone(),
                    }
                    .into_key(),
                    rel_type: "EXECUTED_IN".into(),
                    to: BuildNodeKey::BackendJob {
                        node_label: job_identity.node_label.clone(),
                        identity_props: job_identity.identity_props.clone(),
                    }
                    .into_key(),
                    props: vec![Property("at".into(), GraphValue::String(now.clone()))],
                }))?;
            }

            EventPayload::BuildRunning {
                backend,
                backend_handle,
            } => {
                let job_key = BuildNodeKey::BuildJob {
                    build_id: build_id.clone(),
                };

                // Update state to running. We record the backend and handle so
                // operators can correlate a BuildJob to a specific k8s Job name.
                let state_key = BuildNodeKey::BuildJobState {
                    build_id: build_id.clone(),
                    valid_from: now.clone(),
                };

                graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                    resource_key: job_key.into_key(),
                    state_type_key: BuildNodeKey::State.into_key(),
                    state_instance_key: state_key.into_key(),
                    state_instance_props: vec![
                        Property("phase".into(), GraphValue::String("running".into())),
                        Property("backend".into(), GraphValue::String(backend.clone())),
                        Property(
                            "backend_handle".into(),
                            GraphValue::String(backend_handle.clone()),
                        ),
                        Property("valid_from".into(), GraphValue::String(now.clone())),
                    ],
                }))?;
            }

            EventPayload::BuildCompleted { duration_secs } => {
                let job_key = BuildNodeKey::BuildJob {
                    build_id: build_id.clone(),
                };

                // ── Update anchor node with completion metadata ─────────────────────
                graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: job_key.clone().into_key(),
                    props: vec![
                        Property("completed_at".into(), GraphValue::String(now.clone())),
                        Property(
                            "duration_secs".into(),
                            GraphValue::I64(*duration_secs as i64),
                        ),
                        Property("observed_at".into(), GraphValue::String(now.clone())),
                    ],
                }))?;

                // ── Terminal state node ────────────────────────────────────────────
                let state_key = BuildNodeKey::BuildJobState {
                    build_id: build_id.clone(),
                    valid_from: now.clone(),
                };

                graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                    resource_key: job_key.clone().into_key(),
                    state_type_key: BuildNodeKey::State.into_key(),
                    state_instance_key: state_key.into_key(),
                    state_instance_props: vec![
                        Property("phase".into(), GraphValue::String("succeeded".into())),
                        Property("valid_from".into(), GraphValue::String(now.clone())),
                        Property(
                            "duration_secs".into(),
                            GraphValue::I64(*duration_secs as i64),
                        ),
                    ],
                }))?;
            }

            EventPayload::BuildFailed { reason, stage } => {
                let job_key = BuildNodeKey::BuildJob {
                    build_id: build_id.clone(),
                };

                graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: job_key.clone().into_key(),
                    props: vec![
                        Property("completed_at".into(), GraphValue::String(now.clone())),
                        Property("failure_reason".into(), GraphValue::String(reason.clone())),
                        Property(
                            "failure_stage".into(),
                            GraphValue::String(format!("{stage:?}")),
                        ),
                        Property("observed_at".into(), GraphValue::String(now.clone())),
                    ],
                }))?;

                let state_key = BuildNodeKey::BuildJobState {
                    build_id: build_id.clone(),
                    valid_from: now.clone(),
                };

                graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                    resource_key: job_key.into_key(),
                    state_type_key: BuildNodeKey::State.into_key(),
                    state_instance_key: state_key.into_key(),
                    state_instance_props: vec![
                        Property("phase".into(), GraphValue::String("failed".into())),
                        Property("reason".into(), GraphValue::String(reason.clone())),
                        Property("stage".into(), GraphValue::String(format!("{stage:?}"))),
                        Property("valid_from".into(), GraphValue::String(now.clone())),
                    ],
                }))?;
            }

            EventPayload::BuildCancelled { reason } => {
                let job_key = BuildNodeKey::BuildJob {
                    build_id: build_id.clone(),
                };

                graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: job_key.clone().into_key(),
                    props: vec![
                        Property("completed_at".into(), GraphValue::String(now.clone())),
                        Property("observed_at".into(), GraphValue::String(now.clone())),
                    ],
                }))?;

                let state_key = BuildNodeKey::BuildJobState {
                    build_id: build_id.clone(),
                    valid_from: now.clone(),
                };

                graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                    resource_key: job_key.into_key(),
                    state_type_key: BuildNodeKey::State.into_key(),
                    state_instance_key: state_key.into_key(),
                    state_instance_props: vec![
                        Property("phase".into(), GraphValue::String("cancelled".into())),
                        Property(
                            "reason".into(),
                            GraphValue::String(
                                reason.clone().unwrap_or_else(|| "no reason given".into()),
                            ),
                        ),
                        Property("valid_from".into(), GraphValue::String(now.clone())),
                    ],
                }))?;
            }
        }
    }

    Ok(())
}

// ── Supervisor ─────────────────────────────────────────────────────────────────

/// Supervisor for the Cyclops graph processor.
///
/// Mirrors the structure of `ClusterConsumerSupervisor` in the k8s agent:
/// - Spawns and supervises a Cassini TCP client actor.
/// - On registration, connects to Neo4j and spawns the GraphController.
/// - On message receipt, deserializes the BuildEvent and calls project_event.
///
/// The processor subscribes to all Cyclops outbound subjects. It does not
/// subscribe to `cyclops.build.requested` — that subject is consumed by the
/// orchestrator, not Polar.
pub struct BuildProcessorSupervisor;

pub struct BuildProcessorState {
    graph_config: neo4rs::Config,
    tcp_client: Arc<dyn CassiniClient>,
    graph_controller: Option<GraphController>,
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
}

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

        debug!("Loading graph database configuration");
        let graph_config = get_neo_config()?;

        debug!("Spawning tcp client");
        let tcp_client = polar::cassini::TcpClient::spawn(
            "polar.builds.processor.tcp",
            myself.clone(),
            |event| Some(SupervisorMessage::ClientEvent { event }),
        )
        .await?;

        Ok(BuildProcessorState {
            graph_config,
            tcp_client: Arc::new(tcp_client),
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
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    info!("Cassini client registered — connecting to Neo4j");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);

                    let graph_signal_port = Arc::new(OutputPort::<GraphSignal>::default());
                    graph_signal_port.subscribe(myself.clone(), |signal| {
                        Some(SupervisorMessage::GraphSignal(signal))
                    });
                    state.graph_controller = Actor::spawn_linked(
                        Some("cyclops.processor.graph.controller".to_string()),
                        GraphControllerActor,
                        polar::graph::controller::GraphControllerArgs { signal_port: graph_signal_port },
                        myself.clone().into(),
                    )
                    .await?
                    .0
                    .into();
                    let _ = state.healthcheck.cast(HealthCheckMessage::GraphConnected);

                    if let Err(e) = state.tcp_client.subscribe(SubscribeRequest {
                        topic: BUILD_EVENTS_TOPIC.to_string(),
                        trace_ctx: None,
                    }) {
                        error!("Failed to subscribe to topic {BUILD_EVENTS_TOPIC}. {e}");
                        return Err(e.into());
                    }
                }

                ClientEvent::MessagePublished { topic, payload, .. } => {
                    if state.draining {
                        warn!(
                            "draining -- logging message on topic '{topic}' \
                             ({} bytes) instead of projecting; will be lost on exit",
                            payload.len()
                        );
                        return Ok(());
                    }
                    let Some(controller) = &state.graph_controller else {
                        error!("received message before graph controller was ready");
                        myself.stop(None);
                        return Ok(());
                    };

                    let event = match from_bytes::<BuildEvent, RkyvError>(&payload) {
                        Ok(e) => e,
                        Err(e) => {
                            warn!(
                                topic = %topic,
                                error = %e,
                                "failed to deserialize BuildEvent — dropping message"
                            );
                            return Ok(());
                        }
                    };

                    debug!(
                        topic = %topic,
                        build_id = %event.build_id,
                        "projecting Cyclops event into graph"
                    );

                    if let Err(e) = project_event(&event, controller) {
                        // Log projection failures but do not stop the supervisor.
                        // A single bad event should not interrupt processing of
                        // subsequent events. The event log is the source of truth —
                        // the graph can be rebuilt by replaying from the broker.
                        error!(
                            build_id = %event.build_id,
                            error = %e,
                            "graph projection failed"
                        );
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
            SupervisionEvent::ActorStarted(cell) => {
                info!("child actor started: {:?}", cell.get_name());
            }
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                let actor_name = cell.get_name().unwrap_or_default();
                info!(
                    "child actor terminated: {:?} reason: {:?}",
                    actor_name,
                    reason
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
                error!("child actor failed: {:?} error: {:?}", cell.get_name(), e);
                // BUG FIX: previously logged only, no exit trigger at all.
                if !state.draining {
                    state.draining = true;
                    warn!("entering drain window before exit");
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            SupervisionEvent::ProcessGroupChanged(..) => {}
        }
        Ok(())
    }
}
