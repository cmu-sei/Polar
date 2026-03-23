use async_trait::async_trait;
use tokio::io::AsyncRead;

use crate::error::BackendError;
use crate::types::{BootstrapSpec, BuildSpec};

/// Opaque handle returned by a backend after job submission.
/// The backend is responsible for interpreting it in poll/cancel/log calls.
/// Stored as a plain string in BuildRecord so it survives serialization.
#[derive(Debug, Clone)]
pub struct BackendJobHandle(pub String);

impl BackendJobHandle {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BackendJobHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for BackendJobHandle {
    fn from(s: String) -> Self {
        Self(s)
    }
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
/// The orchestrator uses this trait for both pipeline jobs and bootstrap jobs.
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
    async fn submit(&self, spec: &BuildSpec) -> Result<BackendJobHandle, BackendError>;

    /// Submit a bootstrap job to build a missing pipeline image.
    /// The bootstrap job clones source, evaluates container.dhall via Nix,
    /// and pushes the resulting image to the target registry.
    async fn submit_bootstrap(
        &self,
        spec: &BootstrapSpec,
    ) -> Result<BackendJobHandle, BackendError>;

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
