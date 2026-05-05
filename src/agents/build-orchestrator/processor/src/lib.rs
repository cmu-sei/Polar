use cassini_types::ClientEvent;
use orchestrator_core::{
    events::{BuildEvent, EventPayload},
    types::subjects::BUILD_EVENTS_TOPIC,
};
use polar::{
    RkyvError, SupervisorMessage,
    cassini::{CassiniClient, SubscribeRequest},
    get_neo_config,
    graph::{
        controller::{
            GraphController, GraphControllerActor, GraphControllerMsg, GraphOp, GraphValue,
            IntoGraphKey, Property, rel::BUILT_BY,
        },
        nodes::{builds::BuildNodeKey, git::GitNodeKey},
    },
};
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use rkyv::from_bytes;
use std::sync::Arc;
use tokio::time::error;
use tracing::{debug, error, info, warn};

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
                    info!("Cassini client registered — connecting to Neo4j");

                    let graph = neo4rs::Graph::connect(state.graph_config.clone())?;

                    state.graph_controller = Actor::spawn_linked(
                        Some("cyclops.processor.graph.controller".to_string()),
                        GraphControllerActor,
                        graph,
                        myself.clone().into(),
                    )
                    .await?
                    .0
                    .into();

                    if let Err(e) = state.tcp_client.subscribe(SubscribeRequest {
                        topic: BUILD_EVENTS_TOPIC.to_string(),
                        trace_ctx: None,
                    }) {
                        error!("Failed to subscribe to topic {BUILD_EVENTS_TOPIC}. {e}");
                        return Err(e.into());
                    }
                }

                ClientEvent::MessagePublished { topic, payload, .. } => {
                    let Some(controller) = &state.graph_controller else {
                        error!("received message before graph controller was ready");
                        myself.stop(None);
                        return Ok(());
                    };

                    // Deserialize the BuildEvent from rkyv bytes.
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
        _myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(cell) => {
                info!("child actor started: {:?}", cell.get_name());
            }
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                info!(
                    "child actor terminated: {:?} reason: {:?}",
                    cell.get_name(),
                    reason
                );
            }
            SupervisionEvent::ActorFailed(cell, e) => {
                error!("child actor failed: {:?} error: {:?}", cell.get_name(), e);
            }
            SupervisionEvent::ProcessGroupChanged(..) => {}
        }
        Ok(())
    }
}
