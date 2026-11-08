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

use async_trait::async_trait;
use cassini_client::credit::{Credit, NoRoom, Slot};
use cassini_client::facade::{
    Backoff, FacadeArgs, RefusalWatch, TcpClientFacade, TlsTransportSpawner, TopicGate,
};
pub use cassini_client::{MessageQueue, OfflineBehavior, PublishRequest, QueueEntry};
use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::{ClientEvent, ControlOp, WireTraceCtx};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort};

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

    /// The payload does not fit in one frame: it is refused before it is sent, as the broker would close the
    /// connection on it.
    #[error("the payload is {bytes} bytes, over the {max} a frame carries")]
    TooLarge { bytes: usize, max: usize },

    /// The client's outbox is full (the broker has not acknowledged what it holds) and the caller asked to fail rather
    /// than wait (`OfflineBehavior::Fail`).
    #[error("the client is busy: its outbox is full")]
    Busy,

    /// The client's outbox and buffer are full: the publish, the newest, is refused rather than an older one dropped
    /// (`OfflineBehavior::Queue`). The agent is also told with `ClientEvent::PublishDropped`.
    #[error("the client's outbox and buffer are full: the publish was refused")]
    BufferFull,
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
    /// Whether the connection is registered with the broker right now, when something is
    /// tracking it (the stable front the agent builder starts). `None` for a client that is the
    /// connection itself, which has no such state to read.
    online: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// The room in the front's outbox and buffer, which a publish takes before it is handed over (`credit.rs`).
    /// `None` for a client that is the connection itself, or a stand-in.
    credit: Option<std::sync::Arc<Credit>>,
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
                delivery: cassini_client::DeliveryMode::Broadcast(events_output),
                answers: None,
            },
            supervisor.into(),
        )
        .await?;

        let queue = MessageQueue::from_env();

        Ok(Self {
            inner,
            queue,
            online: None,
            credit: None,
        })
    }

    /// Same real, existing purpose as `spawn` -- a real `TcpClient`
    /// wrapper this agent's own actors keep publishing/subscribing
    /// through exactly as before -- but backed by
    /// `DeliveryMode::Router` internally: real, incoming
    /// `ClientEvent::MessagePublished` events go to `router`, routed
    /// by topic (see `polar-router`'s own crate documentation), while
    /// every other, real, connection-lifecycle `ClientEvent` variant
    /// still reaches `supervisor` exactly as `map_event` describes --
    /// the same, real mapping shape `spawn` already uses, genuinely
    /// unchanged for the caller.
    ///
    /// `map_event` is wrapped internally into the fire-and-forget
    /// `Arc<dyn Fn(ClientEvent)>` shape `DeliveryMode::Router`'s own
    /// `lifecycle` field requires -- the caller never needs to know
    /// that detail.
    pub async fn spawn_with_router<M, F>(
        service_name: &str,
        supervisor: ActorRef<M>,
        map_event: F,
        router: ActorRef<polar_router::RouterMessage>,
    ) -> Result<Self, ActorProcessingErr>
    where
        M: Send + 'static,
        F: Fn(ClientEvent) -> Option<M> + Send + Sync + 'static,
    {
        Self::spawn_with_router_config(
            service_name,
            supervisor,
            map_event,
            router,
            TCPClientConfig::new()?,
            None,
            None,
        )
        .await
    }

    /// As [`spawn_with_router`](Self::spawn_with_router), with the broker coordinates
    /// supplied instead of read from the process environment. The agent builder uses this,
    /// handing the client what it read once into its snapshot, and the check each publish and
    /// subscribe passes before it is sent (`gate`; none admits everything), and who is told what the
    /// agent was refused (`watch`; none tells no one).
    pub async fn spawn_with_router_config<M, F>(
        service_name: &str,
        supervisor: ActorRef<M>,
        map_event: F,
        router: ActorRef<polar_router::RouterMessage>,
        config: TCPClientConfig,
        gate: Option<TopicGate>,
        watch: Option<RefusalWatch>,
    ) -> Result<Self, ActorProcessingErr>
    where
        M: Send + 'static,
        F: Fn(ClientEvent) -> Option<M> + Send + Sync + 'static,
    {
        let supervisor_for_lifecycle = supervisor.clone();
        let lifecycle: std::sync::Arc<dyn Fn(ClientEvent) + Send + Sync> =
            std::sync::Arc::new(move |event: ClientEvent| {
                if let Some(mapped) = map_event(event)
                    && let Err(e) = supervisor_for_lifecycle.send_message(mapped)
                {
                    tracing::warn!(
                        "TcpClient: failed to deliver lifecycle event to supervisor: {e}"
                    );
                }
            });

        // The agent holds the stable front, which keeps the connection up. See
        // cassini_client::facade.
        let online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let credit = std::sync::Arc::new(Credit::default());
        let (inner, _) = Actor::spawn_linked(
            Some(format!("{service_name}.tcp")),
            TcpClientFacade,
            FacadeArgs {
                config,
                delivery: cassini_client::DeliveryMode::Router { router, lifecycle },
                transport_name: format!("{service_name}.tcp.transport"),
                spawner: std::sync::Arc::new(TlsTransportSpawner),
                online: online.clone(),
                credit: credit.clone(),
                backoff: Backoff::default(),
                gate,
                watch,
            },
            supervisor.into(),
        )
        .await?;

        let queue = MessageQueue::from_env();

        Ok(Self {
            inner,
            queue,
            online: Some(online),
            credit: Some(credit),
        })
    }

    /// The real, raw ActorRef<TcpClientMessage> underneath this
    /// wrapper -- needed by anything constructed separately from
    /// TcpClient itself but sharing the same, real Cassini
    /// connection (e.g. polar_crypto::CryptoActorArgs's own
    /// tcp_client field), rather than going through this struct's
    /// own, higher-level CassiniClient methods.
    pub fn as_actor_ref(&self) -> ActorRef<TcpClientMessage> {
        self.inner.clone()
    }

    /// The exact, real inverse of as_actor_ref -- wraps an
    /// already-spawned ActorRef<TcpClientMessage> as a real TcpClient,
    /// with no new, real actor spawned and no real network connection
    /// attempted at all. The real, genuine reason this exists: a
    /// caller (a real test's own fake broker stand-in, or any other,
    /// real, already-spawned TcpClientMessage-compatible actor) that
    /// already has a real ActorRef can still use this struct's own,
    /// higher-level CassiniClient methods, rather than being forced
    /// back down to raw send_message calls.
    pub fn from_actor_ref(inner: ActorRef<TcpClientMessage>) -> Self {
        Self {
            inner,
            queue: None,
            online: None,
            credit: None,
        }
    }

    /// As [`from_actor_ref`](Self::from_actor_ref), for a front that tracks whether the connection
    /// is registered, so each publish can honour its `OfflineBehavior` against it.
    pub fn from_actor_ref_with_state(
        inner: ActorRef<TcpClientMessage>,
        online: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            inner,
            queue: None,
            online: Some(online),
            credit: None,
        }
    }

    /// As [`from_actor_ref_with_state`](Self::from_actor_ref_with_state), for a front whose outbox and buffer are
    /// counted in `credit`, so each publish takes its room before it is handed over.
    pub fn from_actor_ref_with_credit(
        inner: ActorRef<TcpClientMessage>,
        online: std::sync::Arc<std::sync::atomic::AtomicBool>,
        credit: std::sync::Arc<Credit>,
    ) -> Self {
        Self {
            inner,
            queue: None,
            online: Some(online),
            credit: Some(credit),
        }
    }

    /// The front's outbox and buffer, as counted, when it has them.
    pub fn credit(&self) -> Option<std::sync::Arc<Credit>> {
        self.credit.clone()
    }

    /// Whether `req` is to be decided before it is handed over: too large for a frame, or offline when the caller
    /// would rather fail or drop. `Some` is the answer.
    fn decided_offline(&self, req: &PublishRequest) -> Option<Result<(), CassiniClientError>> {
        if req.payload.len() > cassini_types::MAX_PAYLOAD_BYTES {
            return Some(Err(CassiniClientError::TooLarge {
                bytes: req.payload.len(),
                max: cassini_types::MAX_PAYLOAD_BYTES,
            }));
        }
        // The front is always there, so a cast to it always succeeds. Whether the connection is
        // up is what decides these two: `Queue` is handed to the front, which holds it.
        if let Some(online) = &self.online
            && !online.load(std::sync::atomic::Ordering::SeqCst)
        {
            match req.offline_behavior {
                OfflineBehavior::Fail => return Some(Err(CassiniClientError::BrokerUnavailable)),
                OfflineBehavior::Drop => return Some(Ok(())),
                OfflineBehavior::Queue => {}
            }
        }
        None
    }

    fn hand_over(&self, req: PublishRequest, slot: Slot) -> Result<(), CassiniClientError> {
        self.inner
            .send_message(TcpClientMessage::PublishWith {
                topic: req.topic,
                payload: req.payload,
                trace_ctx: req.trace_ctx,
                share_identity: req.share_identity,
                slot,
            })
            .map_err(|_| CassiniClientError::Disconnected)
    }

    /// Publishes, waiting while the client's outbox is full (the broker has not yet acknowledged what it holds), so
    /// pressure slows the caller instead of failing it (docs/architecture/cassini-backpressure.md). A publish too
    /// large for a frame, or one made offline by a caller that would rather fail or drop, is answered at once as
    /// `publish` answers it. For callers that can await; `publish` is for those that cannot.
    pub async fn publish_when_ready(&self, req: PublishRequest) -> Result<(), CassiniClientError> {
        if let Some(decided) = self.decided_offline(&req) {
            return decided;
        }
        let Some(credit) = &self.credit else {
            return self.publish(req);
        };
        credit.wait_for_outbox(req.payload.len()).await;
        self.hand_over(req, Slot::Outbox)
    }
}

#[async_trait]
impl CassiniClient for TcpClient {
    /// Publishes without waiting. With a front that counts its outbox (the agent builder's), the publish takes room
    /// in the outbox, or, when the outbox is full, its `OfflineBehavior` decides: `Queue` takes room in the buffer and
    /// is refused, the newest, with `BufferFull` when that is full too; `Fail` is refused with `Busy`; `Drop` is
    /// discarded and counted (`Credit::dropped`).
    fn publish(&self, req: PublishRequest) -> Result<(), CassiniClientError> {
        if let Some(decided) = self.decided_offline(&req) {
            return decided;
        }
        if let Some(credit) = &self.credit {
            let queue = matches!(req.offline_behavior, OfflineBehavior::Queue);
            return match credit.take(req.payload.len(), queue) {
                Ok(slot) => self.hand_over(req, slot),
                Err(NoRoom::Busy) => match req.offline_behavior {
                    OfflineBehavior::Drop => {
                        credit.count_dropped();
                        tracing::debug!(topic = %req.topic, "the outbox is full: a publish the caller would drop is dropped");
                        Ok(())
                    }
                    _ => Err(CassiniClientError::Busy),
                },
                Err(NoRoom::BufferFull) => {
                    let reason =
                        "the Cassini client's outbox and buffer are full: the newest is refused"
                            .to_string();
                    tracing::warn!(topic = %req.topic, "{reason}");
                    let _ = self.inner.send_message(TcpClientMessage::NotSent {
                        topic: req.topic,
                        reason,
                    });
                    Err(CassiniClientError::BufferFull)
                }
            };
        }

        let result = self.inner.send_message(TcpClientMessage::Publish {
            topic: req.topic.clone(),
            payload: req.payload.clone(),
            trace_ctx: req.trace_ctx,
            share_identity: req.share_identity,
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

// A `CassiniClient` for test harnesses that exercise anything bound against `&dyn CassiniClient` (a projection, an
// event publisher) without a real broker, actor supervision, or `TCPClientConfig`: it performs no I/O, and records
// what was called, in order, for assertion. (cmu-sei/Polar issue 221, 3c3a4b7.)

use std::sync::Mutex;

/// One recorded call to the mock. Kept flat (an enum, not a per-method Vec) so a test asserting "these calls happened,
/// in this order" does not have to interleave several lists back into one timeline by hand.
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

/// A `CassiniClient` that performs no I/O. Every method succeeds immediately with a fixed value and records the call.
/// Interior mutability (`Mutex`) so it is `Send + Sync`, as the trait requires.
///
/// It does not simulate broker failure modes. A test that needs a specific `CassiniClientError` builds it with
/// [`MockCassiniClient::failing_with`].
#[derive(Default)]
pub struct MockCassiniClient {
    calls: Mutex<Vec<RecordedCall>>,
    fail_with: Option<CassiniClientError>,
}

impl MockCassiniClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call returns this error instead of succeeding. Calls are still recorded before the error is returned, so
    /// "did it attempt the right call" and "did it handle the failure" can be asserted from one run.
    pub fn failing_with(err: CassiniClientError) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail_with: Some(err),
        }
    }

    /// Every call made so far, in order.
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("mock mutex poisoned").clone()
    }

    /// The topics published to, in order, ignoring any other traffic between them.
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

/// `CassiniClientError` is not `Clone`; rebuilt by matching rather than changing the type for the sake of a mock.
fn clone_error(err: &CassiniClientError) -> CassiniClientError {
    match err {
        CassiniClientError::NotRegistered => CassiniClientError::NotRegistered,
        CassiniClientError::Disconnected => CassiniClientError::Disconnected,
        CassiniClientError::Serialization(s) => CassiniClientError::Serialization(s.clone()),
        CassiniClientError::BrokerRejected(s) => CassiniClientError::BrokerRejected(s.clone()),
        CassiniClientError::Timeout => CassiniClientError::Timeout,
        CassiniClientError::BrokerUnavailable => CassiniClientError::BrokerUnavailable,
        CassiniClientError::TooLarge { bytes, max } => CassiniClientError::TooLarge {
            bytes: *bytes,
            max: *max,
        },
        CassiniClientError::Busy => CassiniClientError::Busy,
        CassiniClientError::BufferFull => CassiniClientError::BufferFull,
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
mod mock_tests {
    use super::*;

    fn publish_to(topic: &str) -> PublishRequest {
        PublishRequest {
            topic: topic.into(),
            trace_ctx: None,
            payload: b"x".to_vec(),
            offline_behavior: Default::default(),
            share_identity: true,
        }
    }

    #[test]
    fn records_publish_calls_in_order() {
        let mock = MockCassiniClient::new();
        mock.publish(publish_to("a")).unwrap();
        mock.publish(publish_to("b")).unwrap();
        assert_eq!(
            mock.published_topics(),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn failing_client_still_records_the_attempt() {
        let mock = MockCassiniClient::failing_with(CassiniClientError::BrokerUnavailable);
        let result = mock.publish(publish_to("a"));
        assert!(matches!(result, Err(CassiniClientError::BrokerUnavailable)));
        assert_eq!(mock.published_topics(), vec!["a".to_string()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    /// Stands in for the client's front and records what it is sent.
    struct Recorder {
        received: Arc<Mutex<Vec<TcpClientMessage>>>,
    }

    #[async_trait]
    impl Actor for Recorder {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            self.received.lock().unwrap().push(message);
            Ok(())
        }
    }

    async fn client(online: bool) -> (TcpClient, Arc<Mutex<Vec<TcpClientMessage>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (inner, _) = Actor::spawn(
            None,
            Recorder {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();
        let client = TcpClient::from_actor_ref_with_state(inner, Arc::new(AtomicBool::new(online)));
        (client, received)
    }

    fn publish(behavior: OfflineBehavior) -> PublishRequest {
        PublishRequest {
            topic: "t".into(),
            payload: b"p".to_vec(),
            trace_ctx: None,
            offline_behavior: behavior,
            share_identity: true,
        }
    }

    async fn sent(received: &Arc<Mutex<Vec<TcpClientMessage>>>) -> usize {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        received.lock().unwrap().len()
    }

    #[tokio::test]
    async fn while_online_a_publish_is_sent_whatever_its_offline_behavior() {
        for behavior in [
            OfflineBehavior::Fail,
            OfflineBehavior::Drop,
            OfflineBehavior::Queue,
        ] {
            let (client, received) = client(true).await;
            client.publish(publish(behavior)).unwrap();
            assert_eq!(sent(&received).await, 1);
        }
    }

    #[tokio::test]
    async fn while_offline_fail_is_an_error_at_once_and_sends_nothing() {
        let (client, received) = client(false).await;
        assert!(matches!(
            client.publish(publish(OfflineBehavior::Fail)),
            Err(CassiniClientError::BrokerUnavailable)
        ));
        assert_eq!(sent(&received).await, 0);
    }

    #[tokio::test]
    async fn while_offline_drop_succeeds_and_sends_nothing() {
        let (client, received) = client(false).await;
        client.publish(publish(OfflineBehavior::Drop)).unwrap();
        assert_eq!(sent(&received).await, 0);
    }

    #[tokio::test]
    async fn while_offline_queue_is_handed_to_the_front_which_holds_it() {
        let (client, received) = client(false).await;
        client.publish(publish(OfflineBehavior::Queue)).unwrap();
        assert_eq!(sent(&received).await, 1);
    }

    #[tokio::test]
    async fn a_payload_over_what_a_frame_carries_is_refused_with_an_error_and_not_sent() {
        let (client, received) = client(true).await;
        let mut too_large = publish(OfflineBehavior::Queue);
        too_large.payload = vec![0; cassini_types::MAX_PAYLOAD_BYTES + 1];
        assert!(matches!(
            client.publish(too_large),
            Err(CassiniClientError::TooLarge { bytes, max })
                if bytes == cassini_types::MAX_PAYLOAD_BYTES + 1 && max == cassini_types::MAX_PAYLOAD_BYTES
        ));
        let mut largest = publish(OfflineBehavior::Queue);
        largest.payload = vec![0; cassini_types::MAX_PAYLOAD_BYTES];
        client.publish(largest).unwrap();
        assert_eq!(sent(&received).await, 1, "only the one that fits is sent");
    }

    /// A client whose front's outbox holds one message and its buffer one, online.
    async fn bounded_client() -> (TcpClient, Arc<Credit>, Arc<Mutex<Vec<TcpClientMessage>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (inner, _) = Actor::spawn(
            None,
            Recorder {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();
        let one = cassini_client::outbox::OutboxLimits {
            messages: 1,
            bytes: usize::MAX,
        };
        let credit = Arc::new(Credit::new(one, one));
        let client = TcpClient::from_actor_ref_with_credit(
            inner,
            Arc::new(AtomicBool::new(true)),
            credit.clone(),
        );
        (client, credit, received)
    }

    /// What the front was handed: each publish with the room it took, and each refusal it is to tell the agent of.
    async fn handed(received: &Arc<Mutex<Vec<TcpClientMessage>>>) -> Vec<String> {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        received
            .lock()
            .unwrap()
            .iter()
            .map(|m| match m {
                TcpClientMessage::PublishWith { slot, .. } => format!("{slot:?}"),
                TcpClientMessage::NotSent { .. } => "NotSent".to_string(),
                other => other.kind().to_string(),
            })
            .collect()
    }

    #[tokio::test]
    async fn at_the_bound_each_offline_behavior_decides() {
        let (client, credit, received) = bounded_client().await;
        client.publish(publish(OfflineBehavior::Fail)).unwrap();
        // The outbox is full: Fail is busy, Drop is discarded and counted, Queue waits in the buffer...
        assert!(matches!(
            client.publish(publish(OfflineBehavior::Fail)),
            Err(CassiniClientError::Busy)
        ));
        client.publish(publish(OfflineBehavior::Drop)).unwrap();
        assert_eq!(credit.dropped(), 1);
        client.publish(publish(OfflineBehavior::Queue)).unwrap();
        // ... and the buffer full, the newest is refused, and the agent is to be told.
        assert!(matches!(
            client.publish(publish(OfflineBehavior::Queue)),
            Err(CassiniClientError::BufferFull)
        ));
        assert_eq!(handed(&received).await, ["Outbox", "Buffer", "NotSent"]);
        assert_eq!(credit.refused(), 1);
    }

    #[tokio::test]
    async fn publish_when_ready_waits_for_room_and_goes_when_the_broker_acknowledges() {
        let (client, credit, received) = bounded_client().await;
        client
            .publish_when_ready(publish(OfflineBehavior::Queue))
            .await
            .unwrap();
        let waiting = tokio::spawn(async move {
            client
                .publish_when_ready(publish(OfflineBehavior::Fail))
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!waiting.is_finished(), "it waits rather than fail");
        assert_eq!(handed(&received).await, ["Outbox"]);
        // The broker acknowledges the first: the facade gives its room back.
        credit.settle(1);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("woken by the room made")
            .unwrap()
            .unwrap();
        assert_eq!(handed(&received).await, ["Outbox", "Outbox"]);
    }

    #[tokio::test]
    async fn the_connection_state_is_read_each_time() {
        let online = Arc::new(AtomicBool::new(false));
        let received = Arc::new(Mutex::new(Vec::new()));
        let (inner, _) = Actor::spawn(
            None,
            Recorder {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();
        let client = TcpClient::from_actor_ref_with_state(inner, online.clone());
        assert!(client.publish(publish(OfflineBehavior::Fail)).is_err());
        online.store(true, Ordering::SeqCst);
        client.publish(publish(OfflineBehavior::Fail)).unwrap();
        assert_eq!(sent(&received).await, 1);
    }
}
