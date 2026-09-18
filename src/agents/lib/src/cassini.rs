use async_trait::async_trait;
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

        Ok(Self { inner, queue })
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
        let supervisor_for_lifecycle = supervisor.clone();
        let lifecycle: std::sync::Arc<dyn Fn(ClientEvent) + Send + Sync> =
            std::sync::Arc::new(move |event: ClientEvent| {
                if let Some(mapped) = map_event(event)
                    && let Err(e) = supervisor_for_lifecycle.send_message(mapped)
                {
                    tracing::warn!("TcpClient: failed to deliver lifecycle event to supervisor: {e}");
                }
            });

        let config = TCPClientConfig::new()?;

        let (inner, _) = Actor::spawn_linked(
            Some(format!("{service_name}.tcp")),
            TcpClientActor,
            TcpClientArgs {
                config,
                registration_id: None,
                delivery: cassini_client::DeliveryMode::Router { router, lifecycle },
            },
            supervisor.into(),
        )
        .await?;

        let queue = MessageQueue::from_env();

        Ok(Self { inner, queue })
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
}


#[async_trait]
impl CassiniClient for TcpClient {
    fn publish(&self, req: PublishRequest) -> Result<(), CassiniClientError> {
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
