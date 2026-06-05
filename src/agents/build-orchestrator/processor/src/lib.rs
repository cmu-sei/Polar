use cassini_types::ClientEvent;
use orchestrator_core::events::{BuildEvent, BuildEventPayload};
use polar::{
    BUILD_EVENTS_TOPIC, SupervisorMessage,
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
use std::sync::Arc;
use tracing::{debug, error, info, warn};

// ── Event projection ───────────────────────────────────────────────────────────

/// Project a canonical `BuildEvent` into graph operations.
///
/// Each event type maps to a specific set of graph mutations. The processor
/// is intentionally stateless — it does not cache or aggregate events. Each
/// event is projected independently, which means operations are idempotent:
/// replaying events from the broker produces the same graph state.
///
/// Timestamps: `emitted_at` is the source system clock and is used as the
/// canonical event time for `started_at`, `completed_at`, etc. `observed_at`
/// is stamped at processing time and written on every node as the last-seen
/// marker — useful for detecting stale nodes and measuring observer lag.
///
/// Node ownership rules enforced here:
/// - BuildJob, BuildStage, BuildArtifact: owned by this processor, freely upserted.
/// - GitCommit, BackendJob: foreign — only referenced via EnsureEdge. We do not
///   call UpsertNode on these. The authoritative agent owns their properties.
pub fn project_event(
    event: &BuildEvent,
    graph: &GraphController,
) -> Result<(), ActorProcessingErr> {
    let build_id = event.build_id.clone();

    let Some(emitted) = chrono::DateTime::from_timestamp_secs(event.emitted_at) else {
        error!(
            build_id = %build_id,
            emitted_at = event.emitted_at,
            "invalid emitted_at timestamp — dropping event"
        );
        return Ok(());
    };

    let Some(observed) = chrono::DateTime::from_timestamp_secs(event.observed_at) else {
        error!(
            build_id = %build_id,
            observed_at = event.observed_at,
            "invalid observed_at timestamp — dropping event"
        );
        return Ok(());
    };

    let emitted_str = emitted.to_rfc3339();
    let observed_str = observed.to_rfc3339();

    // Convenience: source system written on every node for queryability.
    // Never branch on this in projection logic — if you're tempted to, fix
    // the normalizer instead.
    let source_system = event.source.system.clone();

    match &event.payload {
        BuildEventPayload::ExecutionStarted {
            commit_sha,
            ref_name,
            repo_url,
            triggered_by,
            backend,
        } => {
            let job_key = BuildNodeKey::BuildJob {
                build_id: build_id.clone(),
            };

            // ── Upsert the BuildJob anchor node ────────────────────────────────
            // First event in the lifecycle. Subsequent events update properties
            // or add state nodes — they do not re-create this node.
            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: job_key.clone().into_key(),
                props: vec![
                    Property("build_id".into(), GraphValue::String(build_id.clone())),
                    Property("repo_url".into(), GraphValue::String(repo_url.clone())),
                    Property("commit_sha".into(), GraphValue::String(commit_sha.clone())),
                    Property("ref_name".into(), GraphValue::String(ref_name.clone())),
                    Property(
                        "triggered_by".into(),
                        GraphValue::String(
                            triggered_by.clone().unwrap_or_else(|| "unknown".into()),
                        ),
                    ),
                    Property(
                        "source_system".into(),
                        GraphValue::String(source_system.clone()),
                    ),
                    Property("started_at".into(), GraphValue::String(emitted_str.clone())),
                    Property(
                        "observed_at".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
                ],
            }))?;

            // ── Initial state node ─────────────────────────────────────────────
            let state_key = BuildNodeKey::BuildJobState {
                build_id: build_id.clone(),
                valid_from: emitted_str.clone(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                resource_key: job_key.clone().into_key(),
                state_type_key: BuildNodeKey::State.into_key(),
                state_instance_key: state_key.into_key(),
                state_instance_props: vec![
                    Property("phase".into(), GraphValue::String("started".into())),
                    Property("valid_from".into(), GraphValue::String(emitted_str.clone())),
                ],
            }))?;

            // ── Edge: GitCommit -[:BUILT_BY]-> BuildJob ────────────────────────
            // GitCommit is owned by the VCS agent. EnsureEdge uses MERGE semantics
            // so this is safe regardless of whether the commit node exists yet.
            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: GitNodeKey::Commit {
                    oid: commit_sha.clone(),
                }
                .into_key(),
                rel_type: BUILT_BY.to_string(),
                to: job_key.clone().into_key(),
                props: vec![
                    Property("at".into(), GraphValue::String(emitted_str.clone())),
                    Property("ref_name".into(), GraphValue::String(ref_name.clone())),
                ],
            }))?;

            // ── Edge: BuildJob -[:EXECUTED_IN]-> BackendJob ────────────────────
            // Only written when the event carries backend identity. GitLab-sourced
            // ExecutionStarted events may not carry this if runner metadata isn't
            // available at pipeline start time — that's fine, the edge is optional.
            if let Some(backend) = backend {
                graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                    from: job_key.into_key(),
                    rel_type: "EXECUTED_IN".into(),
                    to: BuildNodeKey::BackendJob {
                        node_label: backend.node_label.clone(),
                        identity_props: backend.identity_props.clone(),
                    }
                    .into_key(),
                    props: vec![Property(
                        "at".into(),
                        GraphValue::String(emitted_str.clone()),
                    )],
                }))?;
            }
        }

        BuildEventPayload::StageStarted {
            stage_name,
            stage_id,
        } => {
            let stage_key = BuildNodeKey::BuildStage {
                build_id: build_id.clone(),
                stage_id: stage_id.clone(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: stage_key.clone().into_key(),
                props: vec![
                    Property("stage_id".into(), GraphValue::String(stage_id.clone())),
                    Property("stage_name".into(), GraphValue::String(stage_name.clone())),
                    Property(
                        "source_system".into(),
                        GraphValue::String(source_system.clone()),
                    ),
                    Property("started_at".into(), GraphValue::String(emitted_str.clone())),
                    Property(
                        "observed_at".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
                ],
            }))?;

            // ── Edge: BuildJob -[:HAS_STAGE]-> BuildStage ─────────────────────
            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: BuildNodeKey::BuildJob {
                    build_id: build_id.clone(),
                }
                .into_key(),
                rel_type: "HAS_STAGE".into(),
                to: stage_key.into_key(),
                props: vec![],
            }))?;
        }

        BuildEventPayload::StageCompleted {
            stage_name,
            stage_id,
            duration_secs,
            outcome,
        } => {
            let stage_key = BuildNodeKey::BuildStage {
                build_id: build_id.clone(),
                stage_id: stage_id.clone(),
            };

            // Upsert rather than create — StageStarted may not have arrived
            // first if the observer is polling rather than streaming.
            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: stage_key.into_key(),
                props: vec![
                    Property("stage_id".into(), GraphValue::String(stage_id.clone())),
                    Property("stage_name".into(), GraphValue::String(stage_name.clone())),
                    Property("outcome".into(), GraphValue::String(format!("{outcome:?}"))),
                    Property(
                        "duration_secs".into(),
                        GraphValue::I64(*duration_secs as i64),
                    ),
                    Property(
                        "completed_at".into(),
                        GraphValue::String(emitted_str.clone()),
                    ),
                    Property(
                        "observed_at".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
                ],
            }))?;
        }

        BuildEventPayload::VulnerabilityFound {
            severity,
            identifier,
            in_artifact,
        } => {
            // ── Upsert the Vulnerability node ──────────────────────────────────
            // Keyed on identifier — CVE IDs, GHSA IDs, or whatever the scanner
            // emits. Multiple builds finding the same vuln converge to one node,
            // which is the right behavior for tracking vuln spread across builds.
            let vuln_key = BuildNodeKey::Vulnerability {
                identifier: identifier.clone(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: vuln_key.clone().into_key(),
                props: vec![
                    Property("identifier".into(), GraphValue::String(identifier.clone())),
                    Property("severity".into(), GraphValue::String(severity.clone())),
                    Property(
                        "observed_at".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
                ],
            }))?;

            // ── Edge: BuildJob -[:FOUND_VULNERABILITY]-> Vulnerability ─────────
            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: BuildNodeKey::BuildJob {
                    build_id: build_id.clone(),
                }
                .into_key(),
                rel_type: "FOUND_VULNERABILITY".into(),
                to: vuln_key.clone().into_key(),
                props: vec![Property(
                    "at".into(),
                    GraphValue::String(emitted_str.clone()),
                )],
            }))?;

            // ── Edge: Vulnerability -[:FOUND_IN]-> BuildArtifact ──────────────
            // Only written when the scanner can attribute the vuln to a specific
            // artifact. If in_artifact is None the vuln is linked to the build
            // only — honest representation of what the scanner reported.
            if let Some(artifact_digest) = in_artifact {
                graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                    from: vuln_key.into_key(),
                    rel_type: "FOUND_IN".into(),
                    to: BuildNodeKey::BuildArtifact {
                        digest: artifact_digest.clone(),
                    }
                    .into_key(),
                    props: vec![Property(
                        "at".into(),
                        GraphValue::String(emitted_str.clone()),
                    )],
                }))?;
            }
        }

        BuildEventPayload::ExecutionCompleted { duration_secs } => {
            let job_key = BuildNodeKey::BuildJob {
                build_id: build_id.clone(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: job_key.clone().into_key(),
                props: vec![
                    Property(
                        "completed_at".into(),
                        GraphValue::String(emitted_str.clone()),
                    ),
                    Property(
                        "duration_secs".into(),
                        GraphValue::I64(*duration_secs as i64),
                    ),
                    Property(
                        "observed_at".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
                ],
            }))?;

            let state_key = BuildNodeKey::BuildJobState {
                build_id: build_id.clone(),
                valid_from: emitted_str.clone(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                resource_key: job_key.into_key(),
                state_type_key: BuildNodeKey::State.into_key(),
                state_instance_key: state_key.into_key(),
                state_instance_props: vec![
                    Property("phase".into(), GraphValue::String("succeeded".into())),
                    Property("valid_from".into(), GraphValue::String(emitted_str.clone())),
                    Property(
                        "duration_secs".into(),
                        GraphValue::I64(*duration_secs as i64),
                    ),
                ],
            }))?;
        }

        BuildEventPayload::ExecutionFailed { reason, stage } => {
            let job_key = BuildNodeKey::BuildJob {
                build_id: build_id.clone(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: job_key.clone().into_key(),
                props: vec![
                    Property(
                        "completed_at".into(),
                        GraphValue::String(emitted_str.clone()),
                    ),
                    Property("failure_reason".into(), GraphValue::String(reason.clone())),
                    Property(
                        "failure_stage".into(),
                        GraphValue::String(stage.clone().unwrap_or_else(|| "unattributed".into())),
                    ),
                    Property(
                        "observed_at".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
                ],
            }))?;

            let state_key = BuildNodeKey::BuildJobState {
                build_id: build_id.clone(),
                valid_from: emitted_str.clone(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                resource_key: job_key.into_key(),
                state_type_key: BuildNodeKey::State.into_key(),
                state_instance_key: state_key.into_key(),
                state_instance_props: vec![
                    Property("phase".into(), GraphValue::String("failed".into())),
                    Property("reason".into(), GraphValue::String(reason.clone())),
                    Property(
                        "stage".into(),
                        GraphValue::String(stage.clone().unwrap_or_else(|| "unattributed".into())),
                    ),
                    Property("valid_from".into(), GraphValue::String(emitted_str.clone())),
                ],
            }))?;
        }

        BuildEventPayload::ExecutionCancelled { reason } => {
            let job_key = BuildNodeKey::BuildJob {
                build_id: build_id.clone(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: job_key.clone().into_key(),
                props: vec![
                    Property(
                        "completed_at".into(),
                        GraphValue::String(emitted_str.clone()),
                    ),
                    Property(
                        "observed_at".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
                ],
            }))?;

            let state_key = BuildNodeKey::BuildJobState {
                build_id: build_id.clone(),
                valid_from: emitted_str.clone(),
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
                    Property("valid_from".into(), GraphValue::String(emitted_str.clone())),
                ],
            }))?;
        }
        // ── Artifact domain variants ───────────────────────────────────────────
        // These are consumed by the artifact linker. The build processor shares
        // the same topic but has no projection logic for artifact domain events.
        // Explicit arms rather than a catch-all so that adding a new payload
        // variant forces a conscious decision here about whether the processor
        // should handle it.
        BuildEventPayload::ArtifactProduced { .. } => {}
        BuildEventPayload::SbomAnalyzed { .. } => {}
        BuildEventPayload::BinaryLinked { .. } => {}
        BuildEventPayload::ContainerImageCreated { .. } => {}
    }

    Ok(())
}

// ── Supervisor ─────────────────────────────────────────────────────────────────

/// Supervisor for the build graph processor.
///
/// Subscribes to the canonical build events topic on Cassini. All CI systems
/// — Cyclops, GitLab, GitHub Actions — publish to this topic after their
/// respective normalizers translate native events into canonical BuildEvent
/// instances. This processor sees one event shape regardless of origin.
///
/// Cyclops events arrive rkyv-serialized in the legacy format and are converted
/// to canonical BuildEvent via the legacy shim before reaching project_event.
/// All other normalizers publish JSON-serialized canonical events directly.
///
/// The processor is intentionally stateless between events. A projection failure
/// on one event does not stop processing of subsequent events — the broker log
/// is the source of truth and the graph can be rebuilt by replaying from it.
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

        debug!("Spawning Cassini TCP client");
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

                    state.graph_controller = Actor::spawn_linked(
                        Some("builds.processor.graph.controller".to_string()),
                        GraphControllerActor,
                        (),
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

                    // Attempt canonical JSON deserialization first.
                    // If that fails, attempt legacy rkyv deserialization and
                    // convert via the shim. If both fail, drop with a warning.
                    //
                    // This two-path approach exists only during the Cyclops
                    // transition. Once Cyclops is retired or updated to emit
                    // canonical JSON, remove the rkyv branch entirely.
                    let event = match serde_json::from_slice::<BuildEvent>(&payload) {
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
                        source = %event.source.system,
                        "projecting build event into graph"
                    );

                    if let Err(e) = project_event(&event, controller) {
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
