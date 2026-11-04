/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

use std::time::Duration;

use async_trait::async_trait;
use rkyv::Archive;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncRead;

use crate::error::BackendError;
use crate::types::BuildSpec;

/// Opaque handle returned by a backend after job submission.
/// The backend is responsible for interpreting it in poll/cancel/log calls.
/// Stored as a plain string in BuildRecord so it survives serialization.
#[derive(Debug, Clone)]
pub struct BackendJobHandle {
    pub name: String,
    pub uid: String,
}

impl std::fmt::Display for BackendJobHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}
/// Returned by the backend after successful job submission.
/// Contains everything the orchestrator needs to track the job
/// and everything the graph processor needs to write provenance edges.
pub struct SubmittedJob {
    /// Opaque handle for poll/cancel/logs operations.
    pub handle: BackendJobHandle,

    /// Backend-specific identity fields for graph correlation.
    /// The orchestrator treats this as opaque and forwards it
    /// into the build_started event without interpreting it.
    pub graph_identity: JobGraphIdentity,
}

/// The minimum identity needed to write a graph edge between
/// a BuildJob and the backend job that executed it.
/// Defined in build-orchestrator-core so the processor can consume it
/// without depending on build-orchestrator-backend-k8s.
#[derive(Debug, Clone, rkyv::Serialize, rkyv::Deserialize, Archive, Serialize, Deserialize)]
pub struct JobGraphIdentity {
    /// The graph node label for this backend job.
    /// e.g. "KubernetesJob", "PodmanJob", "SshBuildJob"
    /// Must match what the backend's own observation agent writes.
    pub node_label: String,

    /// The properties that uniquely identify this node —
    /// the same key/value pairs the observation agent uses in its MERGE.
    /// e.g. [("uid", "abc-123"), ("namespace", "build-orchestrator-builds")]
    pub identity_props: Vec<(String, String)>,

    /// Human-readable display name for log messages.
    pub display_name: String,
}

/// The status of a submitted backend job as reported by a poll call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    /// Accepted by the backend but not yet running.
    Pending,
    /// Actively executing.
    Running,
    /// Completed successfully.
    Succeeded,
    /// Terminated with failure. Reason is backend-specific and best-effort.
    Failed { reason: Option<String> },
    /// Not found — likely GC'd after TTL or never submitted.
    Unknown,
}

/// How long a finished job is kept, by how it ended. A job that failed is kept longer than one that succeeded:
/// its pod and events are what someone reads to find out why, while a success has said all it has to (its logs
/// went to the object store, and its provenance to the graph, as it finished).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub succeeded: Duration,
    pub failed: Duration,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            succeeded: Duration::from_secs(60 * 60),
            failed: Duration::from_secs(24 * 60 * 60),
        }
    }
}

impl Retention {
    /// The longest any finished job is kept: what a backend's own expiry (Kubernetes' TTL) is set to, so that
    /// jobs are still collected if nothing is applying this policy.
    pub fn longest(&self) -> Duration {
        self.succeeded.max(self.failed)
    }
}

/// Boxed async log stream. Erases the backend-specific concrete type.
pub type LogStream = Box<dyn AsyncRead + Send + Unpin>;

/// Abstraction over build execution environments.
///
/// The orchestrator uses this trait for pipeline jobs.
/// Each has its own submit method because their manifests differ significantly
/// (different images, resource requirements, env vars, and job naming).
/// Poll, cancel, and logs work identically for both — the handle is opaque.
///
/// Implementors: KubernetesBackend, (future) SshBackend, PodmanBackend.
/// BuildJobActor holds Arc<dyn BuildBackend> — it never knows the concrete type.
#[async_trait]
pub trait BuildBackend: Send + Sync {
    /// Submit a pipeline job. The pipeline image must already exist in the
    /// registry — call submit_bootstrap first if it does not.
    async fn submit(&self, spec: &BuildSpec) -> Result<SubmittedJob, BackendError>;

    // Not yet: submit a bootstrap job to build a missing pipeline image. The bootstrap job clones source,
    // evaluates container.dhall via Nix, and pushes the resulting image to the target registry.
    //
    // async fn submit_bootstrap(&self, spec: &BootstrapSpec) -> Result<SubmittedJob, BackendError>;

    /// Poll the current status of any submitted job (pipeline or bootstrap).
    async fn poll(&self, handle: &BackendJobHandle) -> Result<JobStatus, BackendError>;

    /// Request cancellation. Best-effort — the backend may not guarantee
    /// immediate termination, particularly for running pods.
    async fn cancel(&self, handle: &BackendJobHandle) -> Result<(), BackendError>;

    /// Stream logs from a running or recently completed job.
    /// May fail if the job has not yet started or logs have been GC'd by TTL.
    async fn logs(&self, handle: &BackendJobHandle) -> Result<LogStream, BackendError>;

    /// Remove the finished jobs that are older than `retention` allows, and return their names. A job that has
    /// not finished is never touched. A backend with nothing to clean up keeps this default.
    async fn reap(&self, _retention: &Retention) -> Result<Vec<String>, BackendError> {
        Ok(Vec::new())
    }

    /// Human-readable backend identifier for structured logs.
    fn name(&self) -> &'static str;
}
