use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Cassini subject namespace for all Cyclops-emitted events.
/// These are the subjects Polar and other consumers subscribe to.
pub mod subjects {
    /// Inbound: Polar scheduler publishes, Cyclops consumes.
    pub const BUILD_REQUESTED: &str = "cyclops.build.requested";

    /// Outbound: emitted when the orchestrator accepts and schedules a build.
    pub const BUILD_STARTED: &str = "cyclops.build.started";

    /// Outbound: emitted when the backend job transitions to running.
    pub const BUILD_RUNNING: &str = "cyclops.build.running";

    /// Outbound: emitted on successful artifact production.
    pub const BUILD_COMPLETED: &str = "cyclops.build.completed";

    /// Outbound: emitted on build failure at any stage.
    pub const BUILD_FAILED: &str = "cyclops.build.failed";

    /// Outbound: emitted when a build is cancelled.
    pub const BUILD_CANCELLED: &str = "cyclops.build.cancelled";
}

/// Envelope wrapping all Cyclops outbound events.
/// Polar ingests this off the broker and uses it to populate the knowledge graph.
#[derive(
    Debug, Clone, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive, Serialize, Deserialize,
)]
pub struct BuildEvent {
    /// Matches the Cassini subject this event was published on.
    pub subject: String,

    /// Stable identifier correlating all events for a single build.
    pub build_id: String,

    /// Wall-clock time the event was emitted by Cyclops.
    pub emitted_at: i64,

    /// Subject-specific payload.
    pub payload: EventPayload,
}

#[derive(
    Debug, Clone, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive, Serialize, Deserialize,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    BuildStarted {
        repo_url: String,
        commit_sha: String,
        requested_by: String,
    },
    BuildRunning {
        backend: String,
        backend_handle: String,
    },
    BuildCompleted {
        artifact_digest: Option<String>,
        target_registry: String,
        duration_secs: u64,
    },
    BuildFailed {
        reason: String,
        stage: FailureStage,
    },
    BuildCancelled {
        reason: Option<String>,
    },
}

#[derive(
    Debug, Clone, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum FailureStage {
    Scheduling,
    Execution,
    ArtifactPublication,
    ProvenanceEmission,
}

impl BuildEvent {
    pub fn build_started(
        build_id: Uuid,
        repo_url: String,
        commit_sha: String,
        requested_by: String,
    ) -> Self {
        Self {
            subject: subjects::BUILD_STARTED.to_string(),
            build_id: build_id.into(),
            emitted_at: chrono::Utc::now().timestamp(),
            payload: EventPayload::BuildStarted {
                repo_url,
                commit_sha,
                requested_by,
            },
        }
    }

    pub fn build_running(build_id: Uuid, backend: String, backend_handle: String) -> Self {
        Self {
            subject: subjects::BUILD_RUNNING.to_string(),
            build_id: build_id.into(),
            emitted_at: chrono::Utc::now().timestamp(),
            payload: EventPayload::BuildRunning {
                backend,
                backend_handle,
            },
        }
    }

    pub fn build_completed(
        build_id: Uuid,
        artifact_digest: Option<String>,
        target_registry: String,
        duration_secs: u64,
    ) -> Self {
        Self {
            subject: subjects::BUILD_COMPLETED.to_string(),
            build_id: build_id.into(),
            emitted_at: chrono::Utc::now().timestamp(),
            payload: EventPayload::BuildCompleted {
                artifact_digest,
                target_registry,
                duration_secs,
            },
        }
    }

    pub fn build_failed(build_id: Uuid, reason: String, stage: FailureStage) -> Self {
        Self {
            subject: subjects::BUILD_FAILED.to_string(),
            build_id: build_id.into(),
            emitted_at: chrono::Utc::now().timestamp(),
            payload: EventPayload::BuildFailed { reason, stage },
        }
    }

    pub fn build_cancelled(build_id: Uuid, reason: Option<String>) -> Self {
        Self {
            subject: subjects::BUILD_CANCELLED.to_string(),
            build_id: build_id.into(),
            emitted_at: chrono::Utc::now().timestamp(),
            payload: EventPayload::BuildCancelled { reason },
        }
    }
}
