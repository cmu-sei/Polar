use orchestrator_core::error::BackendError;
use thiserror::Error;

/// All failure modes the Podman backend can produce.
///
/// Every variant maps to exactly one `BackendError` variant via `From`.
/// The intent is that `BuildJobActor` never sees `PodmanError` directly —
/// it only sees `BackendError`, keeping the actor decoupled from the backend
/// implementation. Internal helper functions within the podman crate use
/// `PodmanError` freely and convert at the trait boundary.
#[derive(Debug, Error)]
pub enum PodmanError {
    /// The Podman socket is not present or the daemon is not running.
    /// This is the `Unreconciled` trigger condition — it indicates the
    /// local execution environment itself is unavailable, not just a
    /// transient API error.
    #[error("podman socket unreachable: {0}")]
    SocketUnreachable(#[source] bollard::errors::Error),

    /// Image pull failed. Could be a missing image, auth failure, or
    /// registry unreachable. The build cannot proceed.
    #[error("image pull failed for {image}: {source}")]
    ImagePullFailed {
        image: String,
        #[source]
        source: bollard::errors::Error,
    },

    /// Container creation failed before the process started.
    #[error("container creation failed: {0}")]
    ContainerCreateFailed(#[source] bollard::errors::Error),

    /// Container was created but could not be started.
    #[error("container start failed for {container_id}: {source}")]
    ContainerStartFailed {
        container_id: String,
        #[source]
        source: bollard::errors::Error,
    },

    /// Poll (inspect) call failed. The container ID may have been GC'd
    /// or the daemon restarted. Callers should treat this as `Unknown`
    /// and transition to `Unreconciled` if the job was in-flight.
    #[error("container inspect failed for {container_id}: {source}")]
    InspectFailed {
        container_id: String,
        #[source]
        source: bollard::errors::Error,
    },

    /// Stop or remove failed during cancellation. Best-effort — the
    /// container may have already exited.
    #[error("container stop/remove failed for {container_id}: {source}")]
    CancellationFailed {
        container_id: String,
        #[source]
        source: bollard::errors::Error,
    },

    /// Log stream could not be attached. The container may not have started
    /// yet or its logs have been removed.
    #[error("log stream unavailable for {container_id}: {source}")]
    LogStreamUnavailable {
        container_id: String,
        #[source]
        source: bollard::errors::Error,
    },

    /// A Podman API response was structurally valid but semantically
    /// unexpected — e.g. a created container with no ID in the response.
    /// These should never happen against a compliant Podman daemon.
    #[error("unexpected podman response: {0}")]
    UnexpectedResponse(String),
}

impl From<PodmanError> for BackendError {
    fn from(e: PodmanError) -> Self {
        match e {
            PodmanError::SocketUnreachable(_) => {
                BackendError::Unreachable(e.to_string())
            }
            PodmanError::ImagePullFailed { .. } => {
                BackendError::SubmissionFailed(e.to_string())
            }
            PodmanError::ContainerCreateFailed(_) => {
                BackendError::SubmissionFailed(e.to_string())
            }
            PodmanError::ContainerStartFailed { .. } => {
                BackendError::SubmissionFailed(e.to_string())
            }
            PodmanError::InspectFailed { .. } => {
                BackendError::PollFailed(e.to_string())
            }
            PodmanError::CancellationFailed { .. } => {
                BackendError::CancellationFailed(e.to_string())
            }
            PodmanError::LogStreamUnavailable { .. } => {
                BackendError::LogStreamUnavailable(e.to_string())
            }
            PodmanError::UnexpectedResponse(_) => {
                // Treat unexpected responses as poll failures — the state
                // is ambiguous and the actor should handle accordingly.
                BackendError::PollFailed(e.to_string())
            }
        }
    }
}
