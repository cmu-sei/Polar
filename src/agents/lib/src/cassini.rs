use async_trait::async_trait;
pub use cassini_client::{MessageQueue, OfflineBehavior, PublishRequest, QueueEntry};
use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::{ClientEvent, ControlOp, WireTraceCtx};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct SubscribeRequest {
    pub topic: String,
    pub trace_ctx: Option<WireTraceCtx>,
}

#[derive(Debug, Clone)]
pub struct UnsubscribeRequest {
    pub topic: String,
    pub trace_ctx: Option<WireTraceCtx>,
}

#[derive(Debug, Clone)]
pub struct ControlRequest {
    pub op: ControlOp,
    pub trace_ctx: Option<WireTraceCtx>,
}

#[derive(Debug, thiserror::Error)]
pub enum CassiniClientError {
    #[error("client is not registered")]
    NotRegistered,

    #[error("client is disconnected")]
    Disconnected,

    #[error("serialization failed: {0}")]
    Serialization(String),

    #[error("broker rejected request: {0}")]
    BrokerRejected(String),

    #[error("request timed out")]
    Timeout,

    #[error("broker is unavailable")]
    BrokerUnavailable,
}

/// Abstraction over the Cassini client operations
pub trait CassiniClient: Send + Sync {
    fn register(&self) -> Result<String, CassiniClientError>;

    fn publish(&self, req: PublishRequest) -> Result<(), CassiniClientError>;

    fn subscribe(&self, req: SubscribeRequest) -> Result<(), CassiniClientError>;

    fn unsubscribe(&self, req: UnsubscribeRequest) -> Result<(), CassiniClientError>;

    fn control(&self, req: ControlRequest) -> Result<(), CassiniClientError>;

    fn disconnect(&self, trace_ctx: Option<WireTraceCtx>) -> Result<(), CassiniClientError>;

    fn list_sessions(
        &self,
        trace_ctx: Option<WireTraceCtx>,
    ) -> Result<Vec<String>, CassiniClientError>;

    fn list_topics(
        &self,
        trace_ctx: Option<WireTraceCtx>,
    ) -> Result<Vec<String>, CassiniClientError>;
}

#[derive(Clone)]
pub struct TcpClient {
    inner: ActorRef<TcpClientMessage>,
    queue: Option<MessageQueue>,
}

impl TcpClient {
    pub async fn spawn<M, F>(
        service_name: &str,
        supervisor: ActorRef<M>,
        map_event: F,
    ) -> Result<Self, ActorProcessingErr>
    where
        M: Send + 'static,
        F: Fn(ClientEvent) -> Option<M> + Send + Sync + 'static,
    {
        let events_output = std::sync::Arc::new(OutputPort::default());
        events_output.subscribe(supervisor.clone(), map_event);

        let config = TCPClientConfig::new()?;

        let (inner, _) = Actor::spawn_linked(
            Some(format!("{service_name}.tcp")),
            TcpClientActor,
            TcpClientArgs {
                config,
                registration_id: None,
                events_output: Some(events_output),
                event_handler: None,
            },
            supervisor.into(),
        )
        .await?;

        let queue = MessageQueue::from_env();

        Ok(Self { inner, queue })
    }
}

#[async_trait]
impl CassiniClient for TcpClient {
    fn publish(&self, req: PublishRequest) -> Result<(), CassiniClientError> {
        let result = self.inner.send_message(TcpClientMessage::Publish {
            topic: req.topic.clone(),
            payload: req.payload.clone(),
            trace_ctx: req.trace_ctx.clone(),
        });

        match result {
            Ok(_) => Ok(()),
            Err(_) => match req.offline_behavior {
                OfflineBehavior::Fail => Err(CassiniClientError::BrokerUnavailable),
                OfflineBehavior::Drop => Ok(()),
                OfflineBehavior::Queue => {
                    if let Some(ref queue) = self.queue {
                        let entry = QueueEntry {
                            topic: req.topic,
                            payload_b64: base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                &req.payload,
                            ),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            attempts: 0,
                        };
                        queue
                            .append(&entry)
                            .map_err(|e| CassiniClientError::Serialization(e.to_string()))
                    } else {
                        Err(CassiniClientError::BrokerUnavailable)
                    }
                }
            },
        }
    }

    fn subscribe(&self, req: SubscribeRequest) -> Result<(), CassiniClientError> {
        self.inner
            .send_message(TcpClientMessage::Subscribe {
                topic: req.topic,
                trace_ctx: req.trace_ctx,
            })
            .map_err(|_e| CassiniClientError::Disconnected)?;
        Ok(())
    }

    fn unsubscribe(&self, req: UnsubscribeRequest) -> Result<(), CassiniClientError> {
        self.inner
            .send_message(TcpClientMessage::UnsubscribeRequest {
                topic: req.topic,
                trace_ctx: req.trace_ctx,
            })
            .map_err(|_e| CassiniClientError::Disconnected)?;
        Ok(())
    }

    fn control(&self, req: ControlRequest) -> Result<(), CassiniClientError> {
        self.inner
            .send_message(TcpClientMessage::ControlRequest {
                op: req.op,
                trace_ctx: req.trace_ctx,
            })
            .map_err(|_e| CassiniClientError::Disconnected)?;
        Ok(())
    }

    fn disconnect(&self, trace_ctx: Option<WireTraceCtx>) -> Result<(), CassiniClientError> {
        self.inner
            .send_message(TcpClientMessage::Disconnect { trace_ctx })
            .map_err(|_e| CassiniClientError::Disconnected)?;
        Ok(())
    }

    fn list_sessions(
        &self,
        trace_ctx: Option<WireTraceCtx>,
    ) -> Result<Vec<String>, CassiniClientError> {
        self.inner
            .send_message(TcpClientMessage::ListSessions { trace_ctx })
            .map_err(|_e| CassiniClientError::Disconnected)?;
        Ok(vec![])
    }

    fn list_topics(
        &self,
        trace_ctx: Option<WireTraceCtx>,
    ) -> Result<Vec<String>, CassiniClientError> {
        self.inner
            .send_message(TcpClientMessage::ListTopics { trace_ctx })
            .map_err(|_e| CassiniClientError::Disconnected)?;
        Ok(vec![])
    }

    fn register(&self) -> Result<String, CassiniClientError> {
        self.inner
            .send_message(TcpClientMessage::Register)
            .map_err(|_e| CassiniClientError::Disconnected)?;
        Err(CassiniClientError::Timeout)
    }
}

// Mock `CassiniClient` for test harnesses that exercise `GraphOperable`
// impls (or anything else bound against `&dyn CassiniClient`) without a
// real broker, actor supervision, or `TCPClientConfig`.
//
// Per issue #221: `TcpClient::spawn` performs a real `Actor::spawn_linked`
// against `TcpClientActor`, requires env-driven broker config, and links
// to a supervisor `ActorRef`. None of that is needed by projection logic
// that only calls through the `CassiniClient` trait -- this type provides
// the trait with zero actor spawn and zero broker config.
//
// Intended location: a shared test-support crate/module both the
// build-processor and kube-consumer harnesses depend on (issue #221 asks
// for "both agents' harnesses can use it") -- not duplicated per-crate.

/// One recorded call to the mock. Kept intentionally flat (an enum, not a
/// per-method Vec) so a test asserting "these calls happened, in this
/// order" doesn't have to interleave several separate lists back into a
/// single timeline by hand.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordedCall {
    Register,
    Publish { topic: String, payload: Vec<u8> },
    Subscribe { topic: String },
    Unsubscribe { topic: String },
    Control,
    Disconnect,
    ListSessions,
    ListTopics,
}

/// A `CassiniClient` that performs no I/O. Every method succeeds
/// immediately with a fixed value and records what was called, in order,
/// for assertion. Interior mutability (`Mutex`, not `RefCell`) specifically
/// so this is `Send + Sync` and usable from `&dyn CassiniClient` in async
/// contexts, matching the trait's own `Send + Sync` supertrait bound.
///
/// Deliberately does not attempt to simulate broker failure modes
/// (`Disconnected`, `Timeout`, `BrokerRejected`, ...) -- if a test needs to
/// exercise a specific `CassiniClientError` path, construct this with
/// `MockCassiniClient::failing_with(err)` rather than adding scenario
/// branching here; keeping this type dumb is the point.
#[derive(Default)]
pub struct MockCassiniClient {
    calls: Mutex<Vec<RecordedCall>>,
    fail_with: Option<CassiniClientError>,
}

impl MockCassiniClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every method call returns this error instead of succeeding. For
    /// testing a caller's error-propagation path specifically -- calls are
    /// still recorded before the error is returned, so "did we attempt the
    /// right call" and "did we handle the failure right" can both be
    /// asserted from the same run.
    pub fn failing_with(err: CassiniClientError) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail_with: Some(err),
        }
    }

    /// Snapshot of every call made so far, in order.
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("mock mutex poisoned").clone()
    }

    /// Convenience for the common case: "did exactly these publishes
    /// happen, in this order, ignoring any control/subscribe traffic
    /// interleaved with them."
    pub fn published_topics(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                RecordedCall::Publish { topic, .. } => Some(topic),
                _ => None,
            })
            .collect()
    }

    fn record(&self, call: RecordedCall) {
        self.calls.lock().expect("mock mutex poisoned").push(call);
    }

    fn maybe_fail(&self) -> Result<(), CassiniClientError> {
        match &self.fail_with {
            Some(err) => Err(clone_error(err)),
            None => Ok(()),
        }
    }
}

/// CassiniClientError doesn't derive Clone (its variants carry owned
/// Strings, which is fine, but nothing declared the derive) -- reconstruct
/// by matching rather than requiring an upstream trait change just for
/// this mock.
fn clone_error(err: &CassiniClientError) -> CassiniClientError {
    match err {
        CassiniClientError::NotRegistered => CassiniClientError::NotRegistered,
        CassiniClientError::Disconnected => CassiniClientError::Disconnected,
        CassiniClientError::Serialization(s) => CassiniClientError::Serialization(s.clone()),
        CassiniClientError::BrokerRejected(s) => CassiniClientError::BrokerRejected(s.clone()),
        CassiniClientError::Timeout => CassiniClientError::Timeout,
        CassiniClientError::BrokerUnavailable => CassiniClientError::BrokerUnavailable,
    }
}

impl CassiniClient for MockCassiniClient {
    fn register(&self) -> Result<String, CassiniClientError> {
        self.record(RecordedCall::Register);
        self.maybe_fail()?;
        Ok("mock-registration-id".to_string())
    }

    fn publish(&self, req: PublishRequest) -> Result<(), CassiniClientError> {
        self.record(RecordedCall::Publish {
            topic: req.topic,
            payload: req.payload,
        });
        self.maybe_fail()
    }

    fn subscribe(&self, req: SubscribeRequest) -> Result<(), CassiniClientError> {
        self.record(RecordedCall::Subscribe { topic: req.topic });
        self.maybe_fail()
    }

    fn unsubscribe(&self, req: UnsubscribeRequest) -> Result<(), CassiniClientError> {
        self.record(RecordedCall::Unsubscribe { topic: req.topic });
        self.maybe_fail()
    }

    fn control(&self, _req: ControlRequest) -> Result<(), CassiniClientError> {
        self.record(RecordedCall::Control);
        self.maybe_fail()
    }

    fn disconnect(&self, _trace_ctx: Option<WireTraceCtx>) -> Result<(), CassiniClientError> {
        self.record(RecordedCall::Disconnect);
        self.maybe_fail()
    }

    fn list_sessions(
        &self,
        _trace_ctx: Option<WireTraceCtx>,
    ) -> Result<Vec<String>, CassiniClientError> {
        self.record(RecordedCall::ListSessions);
        self.maybe_fail()?;
        Ok(Vec::new())
    }

    fn list_topics(
        &self,
        _trace_ctx: Option<WireTraceCtx>,
    ) -> Result<Vec<String>, CassiniClientError> {
        self.record(RecordedCall::ListTopics);
        self.maybe_fail()?;
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_publish_calls_in_order() {
        let mock = MockCassiniClient::new();
        mock.publish(PublishRequest {
            topic: "a".into(),
            trace_ctx: None,
            payload: b"one".to_vec(),
            offline_behavior: Default::default(),
        })
        .unwrap();
        mock.publish(PublishRequest {
            topic: "b".into(),
            trace_ctx: None,
            payload: b"two".to_vec(),
            offline_behavior: Default::default(),
        })
        .unwrap();

        assert_eq!(
            mock.published_topics(),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn failing_client_still_records_the_attempt() {
        let mock = MockCassiniClient::failing_with(CassiniClientError::BrokerUnavailable);
        let result = mock.publish(PublishRequest {
            topic: "a".into(),
            trace_ctx: None,
            payload: b"x".to_vec(),
            offline_behavior: Default::default(),
        });

        assert!(matches!(result, Err(CassiniClientError::BrokerUnavailable)));
        assert_eq!(mock.published_topics(), vec!["a".to_string()]);
    }
}
