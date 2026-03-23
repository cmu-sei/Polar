use async_trait::async_trait;
use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::{ClientEvent, ControlOp, WireTraceCtx};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort};

#[derive(Debug, Clone)]
pub struct PublishRequest {
    pub topic: String,
    pub payload: Vec<u8>,
    pub trace_ctx: Option<WireTraceCtx>,
}

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

        Ok(Self { inner })
    }
}

#[async_trait]
impl CassiniClient for TcpClient {
    fn publish(&self, req: PublishRequest) -> Result<(), CassiniClientError> {
        self.inner
            .send_message(TcpClientMessage::Publish {
                topic: req.topic,
                payload: req.payload.to_vec(),
                trace_ctx: req.trace_ctx,
            })
            .map_err(|_e| CassiniClientError::Disconnected)?;
        Ok(())
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
