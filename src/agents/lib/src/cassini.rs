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
use cassini_client::facade::{
    Backoff, DEFAULT_OFFLINE_BUFFER, FacadeArgs, TcpClientFacade, TlsTransportSpawner,
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
            },
            supervisor.into(),
        )
        .await?;

        let queue = MessageQueue::from_env();

        Ok(Self {
            inner,
            queue,
            online: None,
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
        )
        .await
    }

    /// As [`spawn_with_router`](Self::spawn_with_router), with the broker coordinates
    /// supplied instead of read from the process environment. The agent builder uses this,
    /// handing the client what it read once into its snapshot.
    pub async fn spawn_with_router_config<M, F>(
        service_name: &str,
        supervisor: ActorRef<M>,
        map_event: F,
        router: ActorRef<polar_router::RouterMessage>,
        config: TCPClientConfig,
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
        let (inner, _) = Actor::spawn_linked(
            Some(format!("{service_name}.tcp")),
            TcpClientFacade,
            FacadeArgs {
                config,
                delivery: cassini_client::DeliveryMode::Router { router, lifecycle },
                transport_name: format!("{service_name}.tcp.transport"),
                spawner: std::sync::Arc::new(TlsTransportSpawner),
                online: online.clone(),
                buffer_limit: DEFAULT_OFFLINE_BUFFER,
                backoff: Backoff::default(),
            },
            supervisor.into(),
        )
        .await?;

        let queue = MessageQueue::from_env();

        Ok(Self {
            inner,
            queue,
            online: Some(online),
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
        }
    }
}

#[async_trait]
impl CassiniClient for TcpClient {
    fn publish(&self, req: PublishRequest) -> Result<(), CassiniClientError> {
        // The front is always there, so a cast to it always succeeds. Whether the connection is
        // up is what decides these two: `Queue` is handed to the front, which holds it.
        if let Some(online) = &self.online
            && !online.load(std::sync::atomic::Ordering::SeqCst)
        {
            match req.offline_behavior {
                OfflineBehavior::Fail => return Err(CassiniClientError::BrokerUnavailable),
                OfflineBehavior::Drop => return Ok(()),
                OfflineBehavior::Queue => {}
            }
        }

        let result = self.inner.send_message(TcpClientMessage::Publish {
            topic: req.topic.clone(),
            payload: req.payload.clone(),
            trace_ctx: req.trace_ctx.clone(),
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
