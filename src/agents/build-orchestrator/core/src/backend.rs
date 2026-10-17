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
/// Defined in cyclops-core so the processor can consume it
/// without depending on cyclops-backend-k8s.
#[derive(Debug, Clone, rkyv::Serialize, rkyv::Deserialize, Archive, Serialize, Deserialize)]
pub struct JobGraphIdentity {
    /// The graph node label for this backend job.
    /// e.g. "KubernetesJob", "PodmanJob", "SshBuildJob"
    /// Must match what the backend's own observation agent writes.
    pub node_label: String,

    /// The properties that uniquely identify this node —
    /// the same key/value pairs the observation agent uses in its MERGE.
    /// e.g. [("uid", "abc-123"), ("namespace", "cyclops-builds")]
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

    /// Submit a bootstrap job to build a missing pipeline image.
    /// The bootstrap job clones source, evaluates container.dhall via Nix,
    /// and pushes the resulting image to the target registry.
    // async fn submit_bootstrap(&self, spec: &BootstrapSpec) -> Result<SubmittedJob, BackendError>;

    /// Poll the current status of any submitted job (pipeline or bootstrap).
    async fn poll(&self, handle: &BackendJobHandle) -> Result<JobStatus, BackendError>;

    /// Request cancellation. Best-effort — the backend may not guarantee
    /// immediate termination, particularly for running pods.
    async fn cancel(&self, handle: &BackendJobHandle) -> Result<(), BackendError>;

    /// Stream logs from a running or recently completed job.
    /// May fail if the job has not yet started or logs have been GC'd by TTL.
    async fn logs(&self, handle: &BackendJobHandle) -> Result<LogStream, BackendError>;

    /// Human-readable backend identifier for structured logs.
    fn name(&self) -> &'static str;
}
