//! BuildJobActor — owns the full lifecycle of a single build.
//!
//! # Execution flow
//!
//! The actor drives the build through these phases in order:
//!
//! 1. ResolvingImage  — look up the repo in config to find the pipeline image.
//!                      If an image digest is already known, proceed to phase 3.
//!                      If not, proceed to phase 2.
//!
//! 2. Bootstrapping   — submit a bootstrap Job to build the missing pipeline
//!                      image. Poll until it completes. On success, record the
//!                      produced image digest and proceed to phase 3.
//!
//! 3. Scheduled       — submit the pipeline Job using the resolved image.
//!                      The pipeline image always has /bin/pipeline-runner as
//!                      its entrypoint. The orchestrator does not inject build
//!                      logic — only context via env vars.
//!
//! 4. Running         — poll until the job reaches a terminal state.
//!
//! # Polling model
//!
//! The actor uses send_after to schedule PollStatus messages to itself.
//! There is no separate timer actor. This means polling is paused for the
//! duration of any async operation (submit, cancel) and resumes after.
//! The actor tracks which phase it is polling for via BuildPhase so it
//! knows whether a Succeeded status means "bootstrap done, now run pipeline"
//! or "pipeline done, we're finished".
//!
//! # Credential resolution
//!
//! Credentials are taken from OrchestratorConfig at actor construction time
//! and stored in BuildJobArguments. The actor does not read config after
//! construction — all inputs are fixed at spawn time.
//!
//! This is a deliberate constraint. The config that produced this actor's
//! arguments is the config that governs this build, regardless of any runtime
//! config reload that may happen later. Builds are deterministic with respect
//! to the config snapshot they were started under.

use std::sync::Arc;
use std::time::Instant;

use crate::actors::build_registry::RegistryMessage;
use crate::client::{LogContainer, StorageClient};
use crate::config::OrchestratorConfig;
use orchestrator_core::{
    backend::{BackendJobHandle, BuildBackend, JobStatus},
    types::{BuildRequest, BuildSpec, BuildState, GitCredentials, RegistryCredentials, SourceRef},
};
use polar::cassini::{CassiniClient, PublishRequest};
use polar::{
    BUILD_EVENTS_TOPIC, BackendIdentity, PackageAnalysisStartedPayload, ProvenanceEvent, RkyvError,
};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use rkyv::to_bytes;
use tracing::instrument;

const POLL_INTERVAL_MS: u64 = 5_000;

/// Values for `ProvenanceEvent::ExecutionFailed::stage`. The field is a free
/// string so external CI systems can use their own vocabulary.
const STAGE_RESOLVING_IMAGE: &str = "resolving_image";
const STAGE_SCHEDULING: &str = "scheduling";
const STAGE_EXECUTION: &str = "execution";

// ── Messages ──────────────────────────────────────────────────────────────────

pub enum BuildJobMessage {
    /// Begin execution. Self-sent immediately after pre_start.
    Start,

    /// Poll the backend for a job status update. Self-sent on a timer.
    /// The actor's BuildPhase determines what a terminal status means.
    PollStatus,

    /// External cancellation request. Reachable from any non-terminal state.
    Cancel { reason: Option<String> },
}

// ── Phase tracking ────────────────────────────────────────────────────────────

/// Which backend job the actor is currently waiting on.
///
/// The poll handler is shared between bootstrap and pipeline jobs. Phase tells
/// it what to do when the polled job reaches a terminal state — for a bootstrap
/// job, success means "now submit the pipeline job"; for a pipeline job, success
/// means "we're done".
#[derive(Debug, Clone, PartialEq, Eq)]
enum BuildPhase {
    /// Waiting on the pipeline job to complete.
    Pipeline,
}

// ── Arguments and state ───────────────────────────────────────────────────────

pub struct BuildJobArguments {
    pub request: BuildRequest,
    pub backend: Arc<dyn BuildBackend>,
    pub registry: ActorRef<RegistryMessage>,
    pub publisher: Arc<dyn CassiniClient>,
    /// Snapshot of config at actor spawn time. Used for image resolution and
    /// credential lookup. Not updated if config changes after spawn.
    pub config: Arc<OrchestratorConfig>,
    /// Storage client for uploading logs and manifests to MinIO.
    pub storage: Arc<StorageClient>,
}

pub struct BuildJobState {
    request: BuildRequest,
    backend: Arc<dyn BuildBackend>,
    registry: ActorRef<RegistryMessage>,
    publisher: Arc<dyn CassiniClient>,
    config: Arc<OrchestratorConfig>,
    storage: Arc<StorageClient>,

    /// The backend handle for whatever job is currently running.
    /// Replaced when transitioning from bootstrap to pipeline phase.
    backend_handle: Option<BackendJobHandle>,

    /// The resolved pipeline image digest. Set after image resolution
    /// completes (either found in config or produced by bootstrap).
    /// Required before the pipeline job can be submitted.
    resolved_image: Option<String>,

    /// Which phase of execution the actor is currently in.
    /// Determines how poll results are interpreted.
    phase: Option<BuildPhase>,

    started_at: Instant,

    /// Whether the registry has been told the job is Running. The backend
    /// reports Running on every poll while the job runs; the transition is
    /// sent once.
    running: bool,
}

// ── Actor implementation ──────────────────────────────────────────────────────

pub struct BuildJobActor;

#[ractor::async_trait]
impl Actor for BuildJobActor {
    type Msg = BuildJobMessage;
    type State = BuildJobState;
    type Arguments = BuildJobArguments;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!(
            build_id = %args.request.build_id,
            source = %args.request.source.locator(),
            revision = %args.request.source.revision(),
            "BuildJobActor starting"
        );

        myself.send_message(BuildJobMessage::Start)?;

        Ok(BuildJobState {
            request: args.request,
            backend: args.backend,
            registry: args.registry,
            publisher: args.publisher,
            config: args.config,
            storage: args.storage,
            backend_handle: None,
            resolved_image: None,
            phase: None,
            started_at: Instant::now(),
            running: false,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BuildJobMessage::Start => self.handle_start(myself, state).await?,
            BuildJobMessage::PollStatus => self.handle_poll(myself, state).await?,
            BuildJobMessage::Cancel { reason } => self.handle_cancel(myself, state, reason).await?,
        }
        Ok(())
    }
}

// ── Phase handlers ────────────────────────────────────────────────────────────

impl BuildJobActor {
    /// Publish a lifecycle event, logging rather than failing on error. By
    /// the time this runs the outcome is already recorded in the registry, so
    /// a failed publish must not crash the actor or skip its remaining steps.
    fn report(state: &BuildJobState, event: ProvenanceEvent, what: &str) {
        if let Err(e) = Self::publish_event(&state.publisher, event) {
            tracing::error!(
                build_id = %state.request.build_id,
                error = %e,
                "failed to publish {what} event"
            );
        }
    }

    pub fn publish_event(
        publisher: &Arc<dyn CassiniClient>,
        event: ProvenanceEvent,
    ) -> Result<(), ActorProcessingErr> {
        let req = PublishRequest {
            topic: BUILD_EVENTS_TOPIC.to_string(),
            payload: to_bytes::<RkyvError>(&event)?.to_vec(),
            trace_ctx: None,
            offline_behavior: Default::default(),
        };

        if let Err(e) = publisher.publish(req) {
            Err(ActorProcessingErr::from(e.to_string()))
        } else {
            Ok(())
        }
    }
    /// Entry point. Transitions to ResolvingImage and resolves the image to run:
    /// the request's own image if it names one, otherwise the configured
    /// pipeline image for the git repo.
    async fn handle_start(
        &self,
        myself: ActorRef<BuildJobMessage>,
        state: &mut BuildJobState,
    ) -> Result<(), ActorProcessingErr> {
        let build_id = state.request.build_id;

        self.transition(state, BuildState::ResolvingImage, None, None, None)?;

        let pipeline_image = match (&state.request.pipeline_image, &state.request.source) {
            (Some(image), _) => Some(image.clone()),
            (None, SourceRef::Git { url, .. }) => {
                state.config.pipeline_image_for(url).map(|s| s.to_string())
            }
            // Repo mappings only exist for git sources.
            (None, SourceRef::RegistryArchive { .. }) => None,
        };

        match pipeline_image {
            Some(image) => {
                tracing::info!(build_id = %build_id, image = %image, "pipeline image resolved");
                self.submit_pipeline(myself, state, image).await?;
            }
            None => {
                // Bootstrapping a missing image is not implemented; fail
                // cleanly rather than leave the build hanging.
                tracing::error!(
                    build_id = %build_id,
                    source = %state.request.source.locator(),
                    "no pipeline image in request or config for this source"
                );
                self.fail(
                    state,
                    "no pipeline image configured for source".to_string(),
                    STAGE_RESOLVING_IMAGE,
                )
                .await?;
                myself.stop(Some("no pipeline image".to_string()));
            }
        }

        Ok(())
    }
    /// Submits the pipeline job using a known image digest.
    /// Called either directly from handle_start (image was in config) or
    /// from handle_poll after a bootstrap job succeeds.
    #[instrument(skip(self, myself, state))]
    async fn submit_pipeline(
        &self,
        myself: ActorRef<BuildJobMessage>,
        state: &mut BuildJobState,
        pipeline_image: String,
    ) -> Result<(), ActorProcessingErr> {
        let build_id = state.request.build_id;

        let git_credentials = git_credentials_from_config(&state.config);
        let registry_credentials = registry_credentials_from_config(&state.config);

        let mut spec = BuildSpec::with_pipeline_runner(
            build_id,
            state.request.source.clone(),
            pipeline_image.clone(),
            git_credentials,
            registry_credentials,
            state.request.command.clone(),
        );
        spec.env = state
            .config
            .backend
            .job_env
            .iter()
            .map(|v| (v.name.clone(), v.value.clone()))
            .collect();

        match state.backend.submit(&spec).await {
            Ok(job) => {
                tracing::info!(
                    build_id = %build_id,
                    handle = %job.handle.name,
                    image = %pipeline_image,
                    "pipeline Job submitted"
                );

                state.backend_handle = Some(job.handle.clone());
                state.resolved_image = Some(pipeline_image.clone());
                state.phase = Some(BuildPhase::Pipeline);

                self.transition(
                    state,
                    BuildState::Scheduled,
                    Some(job.handle.name.clone()),
                    Some(pipeline_image.clone()),
                    None,
                )?;

                // Emit the started event now that we have a concrete image and handle.
                // ExecutionStarted is shaped for git builds; archive sources have
                // no commit or ref, so those are left empty rather than invented.
                let (repo_url, commit_sha) = match &state.request.source {
                    SourceRef::Git { url, commit_sha } => (url.clone(), commit_sha.clone()),
                    SourceRef::RegistryArchive { download_url, .. } => {
                        (download_url.clone(), String::new())
                    }
                };
                let event = ProvenanceEvent::ExecutionStarted {
                    build_id: build_id.to_string(),
                    commit_sha,
                    ref_name: state
                        .request
                        .metadata
                        .get("git_ref")
                        .cloned()
                        .unwrap_or_default(),
                    repo_url,
                    triggered_by: Some(state.request.requested_by.clone()),
                    backend: Some(BackendIdentity {
                        node_label: job.graph_identity.node_label.clone(),
                        identity_props: job.graph_identity.identity_props.clone(),
                    }),
                };

                if let Err(e) = Self::publish_event(&state.publisher, event) {
                    // Non-fatal — the build proceeds regardless of event publish failures.
                    tracing::error!(
                        build_id = %build_id,
                        error = %e,
                        "failed to publish build_started event"
                    );
                }

                // For a package archive, record what is being analyzed and by
                // which image. Only the orchestrator knows this authoritatively;
                // the analyzer reports findings against the build_id.
                if let SourceRef::RegistryArchive { purl, sha256, .. } = &state.request.source {
                    let event =
                        ProvenanceEvent::PackageAnalysisStarted(PackageAnalysisStartedPayload {
                            build_id: build_id.to_string(),
                            purl: purl.clone(),
                            archive_sha256: sha256.clone(),
                            analyzer_image: pipeline_image.clone(),
                        });
                    if let Err(e) = Self::publish_event(&state.publisher, event) {
                        tracing::error!(
                            build_id = %build_id,
                            error = %e,
                            "failed to publish package_analysis_started event"
                        );
                    }
                }

                myself.send_after(std::time::Duration::from_millis(POLL_INTERVAL_MS), || {
                    BuildJobMessage::PollStatus
                });
            }

            Err(e) => {
                tracing::error!(build_id = %build_id, error = %e, "pipeline Job submission failed");
                self.fail(state, e.to_string(), STAGE_SCHEDULING).await?;
                myself.stop(Some("pipeline submission failed".to_string()));
            }
        }

        Ok(())
    }

    /// Polls the currently active backend job and advances state accordingly.
    ///
    /// Behaviour on terminal status depends on BuildPhase:
    /// - Bootstrap + Succeeded → extract produced image digest, submit pipeline job.
    /// - Pipeline  + Succeeded → emit completed event, stop actor.
    /// - Any phase + Failed    → emit failed event, stop actor.
    /// TODO: Failure to poll status from the backend should leave the build in an unreconciled state.
    /// Failure to reconcile will just result in marking the job as failed.
    async fn handle_poll(
        &self,
        myself: ActorRef<BuildJobMessage>,
        state: &mut BuildJobState,
    ) -> Result<(), ActorProcessingErr> {
        let build_id = state.request.build_id;

        let handle = match &state.backend_handle {
            Some(h) => h.clone(),
            None => {
                // This is a programming error — PollStatus should never be
                // scheduled without a backend handle being set first.
                tracing::error!(build_id = %build_id, "PollStatus received with no backend handle");
                myself.stop(Some("poll without backend handle".to_string()));
                return Ok(());
            }
        };

        let phase = match &state.phase {
            Some(p) => p.clone(),
            None => {
                tracing::error!(build_id = %build_id, "PollStatus received with no phase set");
                myself.stop(Some("poll without phase".to_string()));
                return Ok(());
            }
        };

        match state.backend.poll(&handle).await {
            Ok(status) => {
                match status {
                    JobStatus::Pending => {
                        // Backend has accepted the job but it hasn't started yet.
                        // This is normal — keep polling.
                        myself
                            .send_after(std::time::Duration::from_millis(POLL_INTERVAL_MS), || {
                                BuildJobMessage::PollStatus
                            });
                    }

                    JobStatus::Running => {
                        // ProvenanceEvent has no "running" variant — ExecutionStarted
                        // already marks the start of execution — so this is a
                        // registry-only transition.
                        if phase == BuildPhase::Pipeline && !state.running {
                            self.transition(state, BuildState::Running, None, None, None)?;
                            state.running = true;
                        }
                        myself
                            .send_after(std::time::Duration::from_millis(POLL_INTERVAL_MS), || {
                                BuildJobMessage::PollStatus
                            });
                    }

                    JobStatus::Succeeded => match phase {
                        BuildPhase::Pipeline => {
                            let duration = state.started_at.elapsed().as_secs();
                            tracing::info!(
                                build_id = %build_id,
                                duration_secs = duration,
                                "pipeline Job succeeded"
                            );

                            self.transition(state, BuildState::Succeeded, None, None, None)?;

                            // Upload logs and manifest before stopping.
                            // These are best-effort — a storage failure does not
                            // retroactively fail a succeeded build, but we log
                            // prominently so the operator knows artifacts are missing.
                            self.upload_artifacts(state, &handle).await;

                            let event = ProvenanceEvent::ExecutionCompleted {
                                build_id: build_id.to_string(),
                                duration_secs: duration,
                            };

                            Self::report(state, event, "execution_completed");

                            myself.stop(Some("BUILD_SUCCEEDED".to_string()));
                        }
                    },

                    JobStatus::Failed { reason } => {
                        let reason_str =
                            reason.unwrap_or_else(|| "unknown backend failure".to_string());
                        tracing::error!(
                            build_id = %build_id,
                            phase = ?phase,
                            reason = %reason_str,
                            "Job failed"
                        );
                        // Upload whatever logs exist before marking failed.
                        // Failed builds are when logs matter most.
                        self.upload_artifacts(state, &handle).await;
                        self.fail(state, reason_str, STAGE_EXECUTION).await?;
                        myself.stop(Some("job failed".to_string()));
                    }

                    JobStatus::Unknown => {
                        // The backend has lost track of this job. This should not
                        // happen in normal operation — the TTL on completed jobs is
                        // one hour, and we expect to poll more frequently than that.
                        // Treat as a hard failure rather than retrying indefinitely.
                        tracing::error!(
                            build_id = %build_id,
                            phase = ?phase,
                            "backend returned Unknown status for job"
                        );
                        self.fail(
                            state,
                            "backend lost track of job".to_string(),
                            STAGE_EXECUTION,
                        )
                        .await?;
                        myself.stop(Some("job unknown".to_string()));
                    }
                }
            }

            Err(e) => {
                // Poll transport error — transient. Log and retry.
                // TODO: A retry budget will be added in a future pass.
                // TODO: When said retry budget fails, we should move to call this build unreconciled
                tracing::warn!(
                    build_id = %build_id,
                    error = %e,
                    "backend poll failed — will retry"
                );
                myself.send_after(std::time::Duration::from_millis(POLL_INTERVAL_MS), || {
                    BuildJobMessage::PollStatus
                });
            }
        }

        Ok(())
    }

    async fn handle_cancel(
        &self,
        myself: ActorRef<BuildJobMessage>,
        state: &mut BuildJobState,
        reason: Option<String>,
    ) -> Result<(), ActorProcessingErr> {
        let build_id = state.request.build_id;
        tracing::info!(build_id = %build_id, "cancellation requested");

        if let Some(handle) = &state.backend_handle {
            if let Err(e) = state.backend.cancel(handle).await {
                // Cancellation failure is not fatal — we still mark the build
                // as cancelled locally. The backend job may continue running
                // until its own TTL fires, but that is acceptable.
                tracing::warn!(
                    build_id = %build_id,
                    error = %e,
                    "backend cancellation returned error — proceeding with local cancellation"
                );
            }
        }

        self.transition(state, BuildState::Cancelled, None, None, reason.clone())?;

        let event = ProvenanceEvent::ExecutionCancelled {
            build_id: build_id.to_string(),
            reason,
        };
        Self::report(state, event, "execution_cancelled");

        myself.stop(Some("cancelled".to_string()));
        Ok(())
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Convenience wrapper for sending a Transition message to the registry
    /// and logging it. Keeps the call sites terse.
    fn transition(
        &self,
        state: &BuildJobState,
        next_state: BuildState,
        backend_handle: Option<String>,
        resolved_image: Option<String>,
        failure_reason: Option<String>,
    ) -> Result<(), ActorProcessingErr> {
        state.registry.send_message(RegistryMessage::Transition {
            build_id: state.request.build_id,
            next_state,
            backend_handle,
            resolved_image,
            failure_reason,
        })?;
        Ok(())
    }

    /// Transition to Failed and emit a failure event. Shared by all failure paths.
    async fn fail(
        &self,
        state: &BuildJobState,
        reason: String,
        stage: &str,
    ) -> Result<(), ActorProcessingErr> {
        self.transition(state, BuildState::Failed, None, None, Some(reason.clone()))?;

        let event = ProvenanceEvent::ExecutionFailed {
            build_id: state.request.build_id.to_string(),
            reason,
            stage: Some(stage.to_string()),
        };
        Self::report(state, event, "execution_failed");

        Ok(())
    }

    /// Upload build artifacts (logs and job manifest) to object storage.
    ///
    /// Called on both Succeeded and Failed — logs are most valuable when a
    /// build fails. Both container logs (pipeline and clone init) are uploaded
    /// separately so the operator can diagnose clone failures independently
    /// of pipeline failures.
    ///
    /// This is explicitly best-effort. Storage failures are logged but do not
    /// affect the build state transition. A build that succeeded but whose
    /// logs failed to upload is still a succeeded build.
    async fn upload_artifacts(&self, state: &BuildJobState, handle: &BackendJobHandle) {
        let build_id = state.request.build_id;

        // ── Pipeline logs ──────────────────────────────────────────────────────
        match state.backend.logs(handle).await {
            Ok(log_stream) => {
                if let Err(e) = state
                    .storage
                    .upload_log(build_id, LogContainer::Pipeline, log_stream)
                    .await
                {
                    tracing::error!(
                        build_id = %build_id,
                        error = %e,
                        "failed to upload pipeline logs to storage"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    build_id = %build_id,
                    error = %e,
                    "could not open pipeline log stream for upload — logs may be unavailable"
                );
            }
        }
    }
}

// ── Credential resolution ─────────────────────────────────────────────────────
// TODO: It has become untenable to resolve secrets on a per agent basis,
// we will move this responsibility to a "secretAgent" that'll be responsible for managing and distributing secrets
/// Resolves git credentials from config.
///
/// Currently returns the single configured credential set for all repos.
/// Per-repo credential selection will be added when the scheduler owns repo
/// mappings and can carry per-repo credential references in the build request.

fn git_credentials_from_config(_config: &OrchestratorConfig) -> GitCredentials {
    GitCredentials::HttpToken {
        secret_name: "some_secret".into(),
    }
}

/// Resolves registry credentials from config.
fn registry_credentials_from_config(_config: &OrchestratorConfig) -> RegistryCredentials {
    RegistryCredentials::DockerConfigJson {
        secret_name: "some_secret".into(),
    }
}

// ── Bootstrap output derivation ───────────────────────────────────────────────

/// Derives the expected image reference for the output of a bootstrap job.
///
/// The bootstrap image and the orchestrator must agree on this naming
/// convention. The convention is:
///   <target_registry>/<repo_slug>:<commit_sha_short>
///
/// where repo_slug is the last path component of the repo URL with
/// non-alphanumeric characters replaced by hyphens.
///
/// This is a temporary measure. The proper solution is for the bootstrap job
/// to write its output digest to a Job annotation or a well-known ConfigMap
/// that the orchestrator reads back after polling succeeds. That eliminates the
/// need for both sides to agree on a naming convention and makes the digest
/// authoritative rather than derived.
///
/// TODO: replace with annotation/ConfigMap read after bootstrap polling.
pub(crate) fn derive_bootstrap_output_image(
    repo_url: &str,
    commit_sha: &str,
    target_registry: &str,
) -> String {
    let repo_slug = repo_url
        .split('/')
        .last()
        .unwrap_or("unknown")
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
        .to_lowercase();

    let short_sha = &commit_sha[..commit_sha.len().min(12)];

    format!("{}/{repo_slug}:{short_sha}", target_registry)
}
