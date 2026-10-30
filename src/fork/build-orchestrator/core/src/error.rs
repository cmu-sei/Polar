use thiserror::Error;

use crate::types::BuildState;

#[derive(Debug, Error)]
pub enum CyclopsError {
    #[error("invalid state transition from {from} to {to}")]
    InvalidStateTransition { from: BuildState, to: BuildState },

    #[error("build {0} not found")]
    BuildNotFound(uuid::Uuid),

    #[error("backend error: {0}")]
    Backend(#[from] BackendError),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("actor error: {0}")]
    Actor(String),

    #[error("configuration error: {0}")]
    Config(String),
}

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("job submission failed: {0}")]
    SubmissionFailed(String),

    #[error("job poll failed: {0}")]
    PollFailed(String),

    #[error("job cancellation failed: {0}")]
    CancellationFailed(String),

    #[error("log stream unavailable: {0}")]
    LogStreamUnavailable(String),

    #[error("backend not reachable: {0}")]
    Unreachable(String),
}
