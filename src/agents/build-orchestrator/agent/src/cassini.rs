use async_trait::async_trait;
use orchestrator_core::events::BuildEvent;

/// Abstraction over the Cassini client publish operation.
///
/// This trait exists so the orchestrator crate does not take a hard compile-time
/// dependency on the Cassini client library. The concrete implementation wraps
/// the Cassini client actor ref and delegates to it. A no-op stub is provided
/// for tests.
#[async_trait]
pub trait CassiniPublisher: Send + Sync {
    async fn publish(&self, event: BuildEvent) -> Result<(), PublishError>;
}

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("serialization failed: {0}")]
    Serialization(String),

    #[error("broker unavailable: {0}")]
    BrokerUnavailable(String),

    #[error("publish timed out")]
    Timeout,
}

/// Stub publisher that logs events to stdout instead of sending to Cassini.
/// Used during development before the Cassini client actor is wired in.
pub struct LoggingPublisher;

#[async_trait]
impl CassiniPublisher for LoggingPublisher {
    async fn publish(&self, event: BuildEvent) -> Result<(), PublishError> {
        let json = serde_json::to_string_pretty(&event)
            .map_err(|e| PublishError::Serialization(e.to_string()))?;
        tracing::info!(
            subject = %event.subject,
            build_id = %event.build_id,
            event_json = %json,
            "[stub] would publish to Cassini"
        );
        Ok(())
    }
}

/// No-op publisher for unit tests.
pub struct NoopPublisher;

#[async_trait]
impl CassiniPublisher for NoopPublisher {
    async fn publish(&self, _event: BuildEvent) -> Result<(), PublishError> {
        Ok(())
    }
}
