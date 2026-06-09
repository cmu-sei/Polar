use chrono::Utc;
use polar::{
    ProvenanceEvent,
    graph::{
        controller::{
            GraphController, GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property,
            rel::{BUILT_BY, IS},
        },
        nodes::{builds::BuildNodeKey, git::GitNodeKey},
    },
};
use ractor::ActorProcessingErr;
use tracing::error;

// ── Event projection ───────────────────────────────────────────────────────────

/// Project a [`ProvenanceEvent`] lifecycle variant into graph operations.
///
/// Handles only execution lifecycle variants — `ExecutionStarted`, `StageStarted`,
/// `StageCompleted`, `ExecutionCompleted`, `ExecutionFailed`, `ExecutionCancelled`,
/// `VulnerabilityFound`. All other variants are routed to the linker by the
/// supervisor before reaching this function and must never appear here.
///
/// The processor is intentionally stateless — each event is projected
/// independently. Operations use MERGE semantics and are idempotent:
/// replaying events from the broker produces the same graph state.
///
/// `observed_at` is stamped at projection time — it records when this agent
/// processed the event, useful for measuring observer lag and detecting stale
/// nodes. It is distinct from `started_at` / `completed_at` which record
/// when the CI system reported the event occurred.
///
/// Node ownership rules:
/// - `BuildJob`, `BuildStage`, `Vulnerability`: owned by this processor, freely upserted.
/// - `GitCommit`, `BackendJob`: foreign — referenced only via `EnsureEdge`.
///   The authoritative agent owns their properties.
pub fn project_event(
    event: &ProvenanceEvent,
    graph: &GraphController,
) -> Result<(), ActorProcessingErr> {
    let observed_str = Utc::now().to_rfc3339();

    match event {
        ProvenanceEvent::ExecutionStarted {
            build_id,
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
                        "observed_at".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
                ],
            }))?;

            // ── Initial state node ─────────────────────────────────────────────
            let state_key = BuildNodeKey::BuildJobState {
                build_id: build_id.clone(),
                valid_from: observed_str.clone(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                resource_key: job_key.clone().into_key(),
                state_type_key: BuildNodeKey::State.into_key(),
                state_instance_key: state_key.into_key(),
                state_instance_props: vec![
                    Property("phase".into(), GraphValue::String("started".into())),
                    Property(
                        "valid_from".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
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
                    Property("at".into(), GraphValue::String(observed_str.clone())),
                    Property("ref_name".into(), GraphValue::String(ref_name.clone())),
                ],
            }))?;

            // ── Edge: BuildJob -[:EXECUTED_IN]-> BackendJob ────────────────────
            // Only written when the event carries backend identity. GitLab-sourced
            // events may not carry this if runner metadata isn't available at
            // pipeline start time — the edge is optional.
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
                        GraphValue::String(observed_str.clone()),
                    )],
                }))?;
            }

            // ── Edge: BuildJob -[:IS]-> BuildExecution ─────────────────────────
            // Establishes the type hierarchy. BuildExecution is the abstract type
            // anchor; BuildJob is the instance. Consistent with how the artifact
            // domain models OCIArtifact -[:IS]-> Artifact.
            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: BuildNodeKey::BuildJob {
                    build_id: build_id.clone(),
                }
                .into_key(),
                rel_type: IS.into(),
                to: BuildNodeKey::BuildExecution.into_key(),
                props: vec![],
            }))?;
        }

        ProvenanceEvent::StageStarted {
            build_id,
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
                        "started_at".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
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

        ProvenanceEvent::StageCompleted {
            build_id,
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
                        GraphValue::String(observed_str.clone()),
                    ),
                    Property(
                        "observed_at".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
                ],
            }))?;
        }

        ProvenanceEvent::VulnerabilityFound {
            build_id,
            severity,
            identifier,
            in_artifact,
        } => {
            // ── Upsert the Vulnerability node ──────────────────────────────────
            // Keyed on identifier — CVE IDs, GHSA IDs, or whatever the scanner
            // emits. Multiple builds finding the same vuln converge to one node,
            // which is correct for tracking vuln spread across builds.
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
                    GraphValue::String(observed_str.clone()),
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
                        content_hash: artifact_digest.clone(),
                    }
                    .into_key(),
                    props: vec![Property(
                        "at".into(),
                        GraphValue::String(observed_str.clone()),
                    )],
                }))?;
            }
        }

        ProvenanceEvent::ExecutionCompleted {
            build_id,
            duration_secs,
        } => {
            let job_key = BuildNodeKey::BuildJob {
                build_id: build_id.clone(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: job_key.clone().into_key(),
                props: vec![
                    Property(
                        "completed_at".into(),
                        GraphValue::String(observed_str.clone()),
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
                valid_from: observed_str.clone(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::UpdateState {
                resource_key: job_key.into_key(),
                state_type_key: BuildNodeKey::State.into_key(),
                state_instance_key: state_key.into_key(),
                state_instance_props: vec![
                    Property("phase".into(), GraphValue::String("succeeded".into())),
                    Property(
                        "valid_from".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
                    Property(
                        "duration_secs".into(),
                        GraphValue::I64(*duration_secs as i64),
                    ),
                ],
            }))?;
        }

        ProvenanceEvent::ExecutionFailed {
            build_id,
            reason,
            stage,
        } => {
            let job_key = BuildNodeKey::BuildJob {
                build_id: build_id.clone(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: job_key.clone().into_key(),
                props: vec![
                    Property(
                        "completed_at".into(),
                        GraphValue::String(observed_str.clone()),
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
                valid_from: observed_str.clone(),
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
                    Property(
                        "valid_from".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
                ],
            }))?;
        }

        ProvenanceEvent::ExecutionCancelled { build_id, reason } => {
            let job_key = BuildNodeKey::BuildJob {
                build_id: build_id.clone(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: job_key.clone().into_key(),
                props: vec![
                    Property(
                        "completed_at".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
                    Property(
                        "observed_at".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
                ],
            }))?;

            let state_key = BuildNodeKey::BuildJobState {
                build_id: build_id.clone(),
                valid_from: observed_str.clone(),
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
                    Property(
                        "valid_from".into(),
                        GraphValue::String(observed_str.clone()),
                    ),
                ],
            }))?;
        }

        // All other variants are routed to the linker by the supervisor.
        // They must never reach project_event. If they do, the supervisor
        // dispatch routing has a bug.
        other => {
            error!(
                variant = ?std::mem::discriminant(other),
                "project_event received non-lifecycle variant — check supervisor dispatch routing"
            );
        }
    }

    Ok(())
}
