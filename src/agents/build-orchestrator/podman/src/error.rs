use orchestrator_core::error::BackendError;
use thiserror::Error;

/// All failure modes the Podman backend can produce.
///
/// Every variant maps to exactly one `BackendError` variant via `From`.
/// Variants that previously wrapped `bollard::errors::Error` have been
/// updated to carry `String` sources now that container/pod lifecycle
/// operations go through the libpod REST API directly. Only the three
/// operations that still use bollard (`ping`, `ensure_image`, `logs`)
/// retain bollard error sources.
#[derive(Debug, Error)]
pub enum PodmanError {
    /// The Podman socket is not present or the daemon is not running.
    #[error("podman socket unreachable: {0}")]
    SocketUnreachable(#[source] bollard::errors::Error),

    /// Image pull failed. Could be a missing image, auth failure, or
    /// registry unreachable.
    #[error("image pull failed for {image}: {source}")]
    ImagePullFailed {
        image: String,
        #[source]
        source: bollard::errors::Error,
    },

    /// A libpod container or pod create request was rejected by the daemon.
    #[error("container creation failed: {0}")]
    ContainerCreateFailed(String),

    /// Container was created but the start request failed.
    #[error("container start failed for {container_id}: {reason}")]
    ContainerStartFailed {
        container_id: String,
        reason: String,
    },

    /// Inspect call failed or returned 404. Callers should treat this as
    /// `Unknown` and transition to `Unreconciled` if the job was in-flight.
    #[error("container inspect failed for {container_id}: {reason}")]
    InspectFailed {
        container_id: String,
        reason: String,
    },

    /// Pod or container stop/remove failed during cancellation.
    #[error("cancellation failed for {container_id}: {reason}")]
    CancellationFailed {
        container_id: String,
        reason: String,
    },

    /// Log stream could not be attached. Still sourced from bollard since
    /// `logs()` in `backend.rs` is the one remaining bollard call.
    #[error("log stream unavailable for {container_id}: {source}")]
    LogStreamUnavailable {
        container_id: String,
        #[source]
        source: bollard::errors::Error,
    },

    /// A libpod API response was structurally valid but semantically
    /// unexpected — e.g. a non-2xx status where success was required,
    /// or a response body that failed to deserialize.
    #[error("unexpected podman response: {0}")]
    UnexpectedResponse(String),

    /// The git-cloner init container exited non-zero. The workspace was not
    /// populated so the pipeline container was never started.
    #[error("init container \"{container_id}\" failed with exit code {exit_code}")]
    InitContainerFailed {
        container_id: String,
        exit_code: i64,
    },
}

impl From<PodmanError> for BackendError {
    fn from(e: PodmanError) -> Self {
        match e {
            PodmanError::SocketUnreachable(_) => BackendError::Unreachable(e.to_string()),
            PodmanError::ImagePullFailed { .. } => BackendError::SubmissionFailed(e.to_string()),
            PodmanError::ContainerCreateFailed(_) => BackendError::SubmissionFailed(e.to_string()),
            PodmanError::ContainerStartFailed { .. } => {
                BackendError::SubmissionFailed(e.to_string())
            }
            PodmanError::InspectFailed { .. } => BackendError::PollFailed(e.to_string()),
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
            PodmanError::InitContainerFailed { .. } => {
                BackendError::SubmissionFailed(e.to_string())
            }
        }
    }
}
