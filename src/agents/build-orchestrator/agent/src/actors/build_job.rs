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
    events::{BuildEvent, FailureStage},
    types::{
        BootstrapSpec, BuildRequest, BuildSpec, BuildState, GitCredentials, RegistryCredentials,
        subjects::BUILD_EVENTS_TOPIC,
    },
};
use polar::RkyvError;
use polar::cassini::{CassiniClient, PublishRequest};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use rkyv::to_bytes;
use tracing::instrument;

const POLL_INTERVAL_MS: u64 = 5_000;

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
    /// Waiting on a bootstrap job to produce the pipeline image.
    Bootstrap,
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
            repo = %args.request.repo_url,
            sha = %args.request.commit_sha,
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
    pub fn publish_event(
        publisher: &Arc<dyn CassiniClient>,
        event: BuildEvent,
    ) -> Result<(), ActorProcessingErr> {
        let req = PublishRequest {
            topic: BUILD_EVENTS_TOPIC.to_string(),
            payload: to_bytes::<RkyvError>(&event)?.to_vec(),
            trace_ctx: None,
        };

        if let Err(e) = publisher.publish(req) {
            Err(ActorProcessingErr::from(e.to_string()))
        } else {
            Ok(())
        }
    }
    /// Entry point. Transitions to ResolvingImage and checks whether the
    /// pipeline image is already known for this repo.
    async fn handle_start(
        &self,
        myself: ActorRef<BuildJobMessage>,
        state: &mut BuildJobState,
    ) -> Result<(), ActorProcessingErr> {
        let build_id = state.request.build_id;

        self.transition(state, BuildState::ResolvingImage, None, None, None)?;

        let pipeline_image = state
            .config
            .pipeline_image_for(&state.request.repo_url)
            .map(|s| s.to_string());

        match pipeline_image {
            Some(image) => {
                // Image is already known — skip bootstrapping entirely.
                tracing::info!(
                    build_id = %build_id,
                    image = %image,
                    "pipeline image resolved from config — skipping bootstrap"
                );
                self.submit_pipeline(myself, state, image).await?;
            }
            None => {
                // No image recorded for this repo. Run a bootstrap job to
                // build it, then come back and submit the pipeline job.
                tracing::info!(
                    build_id = %build_id,
                    repo = %state.request.repo_url,
                    "no pipeline image found for repo — triggering bootstrap"
                );
                self.submit_bootstrap(myself, state).await?;
            }
        }

        Ok(())
    }

    /// Submits the bootstrap job that will build the missing pipeline image.
    async fn submit_bootstrap(
        &self,
        orchestrator_backend_k8smyself: ActorRef<BuildJobMessage>,
        state: &mut BuildJobState,
    ) -> Result<(), ActorProcessingErr> {
        // TODO: I'm not entirely sure implementing this is the right call at all.
        // Yes, the build environment is needed, but it can be left to operators to decide how it gets there for now.
        todo!("Implement bootstrap job submission.");
        // let build_id = state.request.build_id;

        // // Credentials are taken from config. These are the same secrets for
        // // every build — credential selection per-repo comes later when the
        // // scheduler owns repo mappings and can carry per-repo credential refs.
        // let git_credentials = git_credentials_from_config(&state.config);
        // let registry_credentials = registry_credentials_from_config(&state.config);

        // let spec = BootstrapSpec {
        //     build_id,
        //     repo_url: state.request.repo_url.clone(),
        //     commit_sha: state.request.commit_sha.clone(),
        //     bootstrap_image: state.config.bootstrap.builder_image.clone(),
        //     container_config_ref: state.config.bootstrap.container_config_ref.clone(),
        //     target_registry: state.config.bootstrap.target_registry.clone(),
        //     git_credentials,
        //     registry_credentials,
        // };

        // match state.backend.submit_bootstrap(&spec).await {
        //     Ok(handle) => {
        //         tracing::info!(
        //             build_id = %build_id,
        //             handle = %handle,
        //             "bootstrap Job submitted"
        //         );

        //         state.backend_handle = Some(handle.clone());
        //         state.phase = Some(BuildPhase::Bootstrap);

        //         self.transition(
        //             state,
        //             BuildState::Bootstrapping,
        //             Some(handle.to_string()),
        //             None,
        //             None,
        //         )?;

        //         myself.send_after(std::time::Duration::from_millis(POLL_INTERVAL_MS), || {
        //             BuildJobMessage::PollStatus
        //         });
        //     }

        //     Err(e) => {
        //         tracing::error!(build_id = %build_id, error = %e, "bootstrap Job submission failed");
        //         self.fail(state, e.to_string(), FailureStage::Scheduling)
        //             .await?;
        //         myself.stop(Some("bootstrap submission failed".to_string()));
        //     }
        // }
        // Ok(())
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

        let spec = BuildSpec::with_pipeline_runner(
            build_id,
            state.request.repo_url.clone(),
            state.request.commit_sha.clone(),
            pipeline_image.clone(),
            git_credentials,
            registry_credentials,
        );

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
                let event = BuildEvent::build_started(
                    build_id,
                    state.request.repo_url.clone(),
                    state.request.commit_sha.clone(),
                    state.request.requested_by.clone(),
                    job.graph_identity.clone(),
                );

                if let Err(e) = Self::publish_event(&state.publisher, event) {
                    // Non-fatal — the build proceeds regardless of event publish failures.
                    tracing::error!(
                        build_id = %build_id,
                        error = %e,
                        "failed to publish build_started event"
                    );
                }

                myself.send_after(std::time::Duration::from_millis(POLL_INTERVAL_MS), || {
                    BuildJobMessage::PollStatus
                });
            }

            Err(e) => {
                tracing::error!(build_id = %build_id, error = %e, "pipeline Job submission failed");
                self.fail(state, e.to_string(), FailureStage::Scheduling)
                    .await?;
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
                        if phase == BuildPhase::Pipeline {
                            // Only emit running events for pipeline jobs.
                            // Bootstrap running is an internal implementation detail
                            // that Polar does not need to track as a separate state.
                            self.transition(state, BuildState::Running, None, None, None)?;
                            let event = BuildEvent::build_running(
                                build_id,
                                state.backend.name().to_string(),
                                handle.to_string(),
                            );

                            Self::publish_event(&state.publisher, event)?;
                        }
                        myself
                            .send_after(std::time::Duration::from_millis(POLL_INTERVAL_MS), || {
                                BuildJobMessage::PollStatus
                            });
                    }

                    JobStatus::Succeeded => match phase {
                        BuildPhase::Bootstrap => {
                            // Bootstrap complete. The bootstrap job is responsible for
                            // writing the produced image digest somewhere the orchestrator
                            // can read it back. For now we construct the expected image
                            // ref deterministically — the bootstrap image pushes to a
                            // known location based on repo URL and commit SHA.
                            //
                            // TODO: in a future pass the bootstrap job should write its
                            // output digest to a well-known ConfigMap or Job annotation
                            // that the orchestrator reads here. For now we derive the
                            // expected ref, which requires the bootstrap image and the
                            // orchestrator to agree on the naming convention.
                            let produced_image = derive_bootstrap_output_image(
                                &state.request.repo_url,
                                &state.request.commit_sha,
                                &state.config.bootstrap.target_registry,
                            );

                            tracing::info!(
                                build_id = %build_id,
                                image = %produced_image,
                                "bootstrap Job succeeded — submitting pipeline Job"
                            );

                            self.submit_pipeline(myself, state, produced_image).await?;
                        }

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

                            let event = BuildEvent::build_completed(build_id, duration);

                            Self::publish_event(&state.publisher, event)?;

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
                        self.fail(state, reason_str, FailureStage::Execution)
                            .await?;
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
                            FailureStage::Execution,
                        )
                        .await?;
                        myself.stop(Some("job unknown".to_string()));
                    }
                }
            }

            Err(e) => {
                // Poll transport error — transient. Log and retry.
                // A retry budget will be added in a future pass.
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

        let event = BuildEvent::build_cancelled(build_id, reason);
        Self::publish_event(&state.publisher, event)?;

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
        stage: FailureStage,
    ) -> Result<(), ActorProcessingErr> {
        self.transition(state, BuildState::Failed, None, None, Some(reason.clone()))?;

        let event = BuildEvent::build_failed(state.request.build_id, reason, stage);
        Self::publish_event(&state.publisher, event)?;

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

/// Resolves git credentials from config.
///
/// Currently returns the single configured credential set for all repos.
/// Per-repo credential selection will be added when the scheduler owns repo
/// mappings and can carry per-repo credential references in the build request.
fn git_credentials_from_config(config: &OrchestratorConfig) -> GitCredentials {
    GitCredentials::HttpToken {
        secret_name: config.credentials.git_secret_name.clone(),
    }
}

/// Resolves registry credentials from config.
fn registry_credentials_from_config(config: &OrchestratorConfig) -> RegistryCredentials {
    RegistryCredentials::DockerConfigJson {
        secret_name: config.credentials.registry_secret_name.clone(),
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
fn derive_bootstrap_output_image(
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
