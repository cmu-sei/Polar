use crate::UNEXPECTED_MESSAGE_STR;
use crate::{
    PUBLISH_REQ_FAILED_TXT, REGISTRATION_REQ_FAILED_TXT,
    SESSION_MISSING_REASON_STR, SESSION_NOT_FOUND_TXT, SUBSCRIBE_REQUEST_FAILED_TXT,
};
use async_trait::async_trait;
use cassini_types::{ArchivedClientMessage, BrokerMessage, ClientMessage, DisconnectReason, WireTraceCtx, ShutdownPhase, ControlOp, ControlResult, ControlError};
use ractor::{registry::where_is, rpc::CallResult, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use rkyv::{
    deserialize,
    rancor::{self, Error, Source},
};
use rustls::{
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
    server::WebPkiClientVerifier,
    RootCertStore, ServerConfig,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::{
    io::{split, AsyncReadExt, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task::JoinHandle,
};
use tokio_rustls::{server::TlsStream, TlsAcceptor};
use tracing::{debug, debug_span, error, info, info_span, instrument, trace, trace_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use std::collections::HashMap;
use tokio_util::sync::CancellationToken;
use cassini_tracing::try_set_parent_otel;
use uuid::Uuid;

static ZERO_LEN_LAST_LOG_MS: AtomicU64 = AtomicU64::new(0);
static ZERO_LEN_COUNT: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ============================== Listener Manager ============================== //

pub struct ListenerManager;

pub struct ListenerManagerState {
    bind_addr: String,
    server_config: Arc<ServerConfig>,
    is_shutting_down: bool,
    terminating_listeners: bool,
    accept_task_abort: Option<tokio::task::AbortHandle>,
    shutdown_token: CancellationToken,
    listeners: HashMap<String, ActorRef<BrokerMessage>>, // client_id -> listener ref
    session_mgr: ActorRef<BrokerMessage>,
}

pub struct ListenerManagerArgs {
    pub bind_addr: String,
    pub server_cert_file: String,
    pub private_key_file: String,
    pub ca_cert_file: String,
    pub session_mgr: ActorRef<BrokerMessage>,
    pub shutdown_auth_token: Option<String>,
}

#[async_trait]
impl Actor for ListenerManager {
    type Msg = BrokerMessage;
    type State = ListenerManagerState;
    type Arguments = ListenerManagerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ListenerManagerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        let span = debug_span!("cassini.listener_manager.init", actor = ?myself, bind_addr = %args.bind_addr);
        let _g = span.enter();

        let _shutdown_auth_token = args.shutdown_auth_token.clone();

        debug!("ListenerManager starting");

        // install default crypto provider
        let provider = rustls::crypto::aws_lc_rs::default_provider().install_default();
        if provider.is_err() {
            debug!("Crypto provider already configured");
        } else {
            debug!("Crypto provider configured");
        }

        debug!("Gathering certificates for mTLS");
        let certs: Vec<_> = CertificateDer::pem_file_iter(&args.server_cert_file)
            .map_err(|e| ActorProcessingErr::from(e))?
            .map(|cert| cert.map_err(|e| ActorProcessingErr::from(e)))
            .collect::<Result<Vec<_>, _>>()?;

        let mut root_store = RootCertStore::empty();
        let root_cert = CertificateDer::from_pem_file(&args.ca_cert_file).map_err(|e| {
            error!(error = ?e, ca_cert_file = %args.ca_cert_file, "Failed reading CA cert PEM");
            ActorProcessingErr::from(e)
        })?;
        root_store
            .add(root_cert)
            .map_err(|e| ActorProcessingErr::from(e))?;

        let verifier = WebPkiClientVerifier::builder(Arc::new(root_store))
            .build()
            .map_err(|e| ActorProcessingErr::from(e))?;

        let private_key = PrivateKeyDer::from_pem_file(&args.private_key_file).map_err(|e| {
            error!(error = ?e, private_key_file = %args.private_key_file, "Failed reading server key PEM");
            ActorProcessingErr::from(e)
        })?;

        debug!("Building ServerConfig for mTLS");

        let server_config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, private_key)
            .map_err(|e| ActorProcessingErr::from(e))?;

        Ok(ListenerManagerState {
            bind_addr: args.bind_addr,
            server_config: Arc::new(server_config),
            is_shutting_down: false,
            accept_task_abort: None,
            shutdown_token: CancellationToken::new(),
            terminating_listeners: false,
            listeners: HashMap::new(),
            session_mgr: args.session_mgr,
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let span = info_span!(
            "cassini.listener_manager.serve",
            actor = ?myself,
            bind_addr = %state.bind_addr
        );
        let _g = span.enter();

        let bind_addr = state.bind_addr.clone();
        let acceptor = TlsAcceptor::from(Arc::clone(&state.server_config));
        let server = TcpListener::bind(&bind_addr)
            .await
            .map_err(|e| ActorProcessingErr::from(e))?;

        info!("Server running on {bind_addr}");
        let shutdown_token = state.shutdown_token.clone();
        let session_mgr = state.session_mgr.clone(); // clone for use in spawned tasks

        // Accept loop runs in its own task; do NOT keep an entered span alive forever.
        let join_handle = tokio::spawn({
            let myself = myself.clone();
            let session_mgr = session_mgr; // move into closure
            async move {
                loop {
                    let (stream, peer_addr) = match server.accept().await {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(error = %e, "TCP accept failed");
                            continue;
                        }
                    };

                    // Check if we're shutting down before accepting new connection
                    if shutdown_token.is_cancelled() {
                        debug!("ListenerManager not accepting new connections, stopping accept loop");
                        break;
                    }

                    let handshake_span = info_span!(
                        "cassini.listener_manager.accept",
                        peer_addr = %peer_addr
                    );

                    let acceptor = acceptor.clone();
                    let myself = myself.clone();
                    let session_mgr = session_mgr.clone(); // clone again for inner spawn

                    tokio::spawn(async move {
                        let _g = handshake_span.enter();

                        let stream = match acceptor.accept(stream).await {
                            Ok(s) => s,
                            Err(e) => {
                                warn!(error = %e, "TLS handshake failed");
                                return;
                            }
                        };

                        let client_id = Uuid::new_v4().to_string();
                        let (reader, writer) = split(stream);
                        let writer = BufWriter::new(writer);

                        let listener_args = ListenerArguments {
                            writer: Arc::new(Mutex::new(writer)),
                            reader: Some(reader),
                            client_id: client_id.clone(),
                            registration_id: None,
                            session_mgr: session_mgr,
                            broker: myself.try_get_supervisor().expect("ListenerManager must have a supervisor (the broker)").into(),
                        };

                        let spawn_span = info_span!(
                            "cassini.listener_manager.spawn_listener",
                            client_id = %client_id
                        );
                        let _g2 = spawn_span.enter();

                        match Actor::spawn_linked(
                            Some(client_id.clone()),
                            Listener,
                            listener_args,
                            myself.clone().into(),
                        )
                        .await
                        {
                            Ok((listener_ref, _)) => {
                                let _ = myself.send_message(BrokerMessage::RegisterListener {
                                    client_id: client_id.clone(),
                                    listener_ref,
                                });
                            }
                            Err(e) => {
                                error!(error = ?e, "Failed to spawn Listener actor for connection {}", client_id);
                            }
                        }
                    });
                }
            }
        });

        state.accept_task_abort = Some(join_handle.abort_handle());

        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(actor_cell) => {
                debug!(
                    worker_name = ?actor_cell.get_name(),
                    worker_id = ?actor_cell.get_id(),
                    "Worker started"
                );
                if actor_cell.get_name().is_some() {
                    // Actor ref will be registered via RegisterListener message after spawn.
                }
            }
            SupervisionEvent::ActorTerminated(actor_cell, _, _) => {
                if let Some(name) = actor_cell.get_name() {
                    state.listeners.remove(&name);
                    // If we are in TerminateListeners phase and no listeners remain, stop manager
                    if state.terminating_listeners && state.listeners.is_empty() {
                        if let Some(supervisor) = myself.try_get_supervisor() {
                            let _ = supervisor.send_message(BrokerMessage::ShutdownPhaseComplete {
                                phase: ShutdownPhase::TerminateListeners,
                            });
                        }
                        myself.stop(Some("SHUTDOWN_LISTENERS_TERMINATED".to_string()));
                    }
                }
            }
            _ => (),
        }
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<BrokerMessage>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            // Phase 1: stop accepting new connections, but keep existing listeners alive
            BrokerMessage::InitiateShutdownPhase { phase } if phase == ShutdownPhase::StopAcceptingNewConnections => {
                self.handle_initiate_shutdown_phase_stop_accepting(myself.clone(), state).await?;
            }
            // Final phase: terminate all listener actors and then stop
            BrokerMessage::InitiateShutdownPhase { phase } if phase == ShutdownPhase::TerminateListeners => {
                self.handle_initiate_shutdown_phase_terminate_listeners(myself.clone(), state).await?;
            }
            BrokerMessage::RegisterListener { client_id, listener_ref } => {
                state.listeners.insert(client_id, listener_ref);
            }
            BrokerMessage::RegistrationResponse { client_id, result, trace_ctx, } => {
                self.handle_registration_response(myself.clone(), state, client_id, result, trace_ctx).await?;
            }
            BrokerMessage::DisconnectRequest { reason, client_id, registration_id, trace_ctx } => {
                self.handle_disconnect_request(myself.clone(), state, reason, client_id, registration_id, trace_ctx).await?;
            }
            _ => (),
        }
        Ok(())
    }
}

impl ListenerManager {
    // ===== Handler methods for ListenerManager =====
    async fn handle_initiate_shutdown_phase_stop_accepting(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut ListenerManagerState,
    ) -> Result<(), ActorProcessingErr> {
        if state.is_shutting_down {
            warn!("ListenerManager already shutting down");
            return Ok(());
        }

        info!("ListenerManager: stopping new connections");
        state.is_shutting_down = true;

        // Cancel the accept loop
        state.shutdown_token.cancel();
        if let Some(handle) = state.accept_task_abort.take() {
            handle.abort();
        }

        // Signal that we have completed the phase
        if let Some(supervisor) = myself.try_get_supervisor() {
            supervisor.send_message(BrokerMessage::ShutdownPhaseComplete {
                phase: ShutdownPhase::StopAcceptingNewConnections,
            })?;
        }
        // DO NOT STOP the manager; keep it alive for the final phase.
        Ok(())
    }

    async fn handle_initiate_shutdown_phase_terminate_listeners(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut ListenerManagerState,
    ) -> Result<(), ActorProcessingErr> {
        if state.terminating_listeners {
            return Ok(());
        }
        info!("ListenerManager: terminating all listener actors");
        state.terminating_listeners = true;

        // Stop all active listeners
        for (client_id, listener_ref) in &state.listeners {
            let listener = listener_ref.clone();
                listener.stop(Some("SHUTDOWN_TERMINATE_LISTENER".to_string()));
            debug!("Stopped listener {}", client_id);
        }

        // If no listeners were active, complete immediately
        if state.listeners.is_empty() {
            if let Some(supervisor) = myself.try_get_supervisor() {
                let _ = supervisor.send_message(BrokerMessage::ShutdownPhaseComplete {
                    phase: ShutdownPhase::TerminateListeners,
                });
            }
            myself.stop(Some("SHUTDOWN_NO_LISTENERS".to_string()));
        }
        // Otherwise, wait for ActorTerminated events to trigger completion
        Ok(())
    }

    async fn handle_registration_response(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerManagerState,
        client_id: String,
        result: Result<String, String>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        // Reject new registrations during shutdown
        if state.is_shutting_down {
            warn!("Rejecting registration response during shutdown");
            return Ok(());
        }

        let span = trace_span!("cassini.request.registration", %client_id);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();

        trace!("Forwarding registration response to listener");

        if let Some(listener_ref) = state.listeners.get(&client_id) {
            if let Err(e) = listener_ref.send_message(BrokerMessage::RegistrationResponse {
                client_id,
                result,
                trace_ctx: Some(span.context()),
            }) {
                error!(error = %e, "Failed to forward RegistrationResponse to listener");
            }
        } else {
            warn!("Couldn't find listener {} in listeners map", client_id);
        }
        Ok(())
    }

    async fn handle_disconnect_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerManagerState,
        reason: DisconnectReason,
        client_id: String,
        registration_id: Option<String>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.listener_manager.disconnect_request", %client_id, registration_id = ?registration_id, reason = ?reason);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("ListenerManager handling disconnect");

        // Always notify the session if we have a registration_id
        if let Some(id) = registration_id {
            if let Some(session) = where_is(id.clone()) {
                match reason {
                    DisconnectReason::RemoteClosed => {
                        info!("Client disconnected (remote closed)");
                        let _ = session.send_message(BrokerMessage::DisconnectRequest {
                            reason,
                            client_id: client_id.clone(),
                            registration_id: Some(id),
                            trace_ctx: Some(span.context()),
                        });
                    }
                    DisconnectReason::TransportError(ref err) => {
                        warn!("Client disconnected unexpectedly; notifying session");
                        let _ = session.send_message(BrokerMessage::TimeoutMessage {
                            client_id: client_id.clone(),
                            registration_id: id,
                            error: Some(err.clone()),
                        });
                    }
                }
            } else {
                warn!("Session not found for registration_id: {}", id);
            }
        } else {
            warn!("DisconnectRequest missing registration_id");
        }

        // Optionally stop the listener if it still exists (it is already stopping itself)
        if let Some(listener_ref) = state.listeners.get(&client_id) {
            listener_ref.stop(None);
            debug!("Stopped listener {}", client_id);
        }

        // If we are in TerminateListeners phase and this listener was the last one,
        // the ActorTerminated handler will take care of completion.
        Ok(())
    }
}

// ============================== Listener actor ============================== //

struct Listener;

struct ListenerState {
    writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    client_id: String,
    session_mgr: ActorRef<BrokerMessage>,
    session_ref: Option<ActorRef<BrokerMessage>>,
    registration_id: Option<String>,
    task_handle: Option<JoinHandle<()>>,
    broker: ActorRef<BrokerMessage>,
}

struct ListenerArguments {
    writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    client_id: String,
    session_mgr: ActorRef<BrokerMessage>,
    registration_id: Option<String>,
    broker: ActorRef<BrokerMessage>,
}

impl Listener {
    /// Write a framed ClientMessage to the peer.
    ///
    /// Header is **u32 big-endian length**, followed by payload bytes.
    #[instrument(level = "trace", skip(writer, message), fields(client_id = %client_id))]
    async fn write(
        client_id: String,
        message: ClientMessage,
        writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    ) -> Result<(), Error> {
        let bytes = rkyv::to_bytes::<Error>(&message)?;

        let len_u32: u32 = bytes
            .len()
            .try_into()
            .map_err(|_| rancor::Error::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame too large for u32 length",
            )))?;

        trace!(
            msg_ty = %std::any::type_name::<ClientMessage>(),
            len = bytes.len(),
            first_8 = ?&bytes[..bytes.len().min(8)],
            "Sending frame"
        );

        let mut w = writer.lock().await;

        w.write_all(&len_u32.to_be_bytes())
            .await
            .map_err(rancor::Error::new)?;
        w.write_all(&bytes).await.map_err(rancor::Error::new)?;
        w.flush().await.map_err(rancor::Error::new)?;

        Ok(())
    }
}

#[async_trait]
impl Actor for Listener {
    type Msg = BrokerMessage;
    type State = ListenerState;
    type Arguments = ListenerArguments;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ListenerArguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!(
            actor = ?myself,
            client_id = %args.client_id,
            "Listener pre_start"
        );

        Ok(ListenerState {
            writer: args.writer,
            reader: args.reader,
            client_id: args.client_id,
            session_mgr: args.session_mgr,
            session_ref: None,
            registration_id: args.registration_id,
            task_handle: None,
            broker: args.broker,
        })
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let Some(handle) = state.task_handle.take() {
            handle.abort();
        }
        Ok(())
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // short-lived startup span only
        {
            let span = info_span!("cassini.listener.start", client_id = %state.client_id);
            let _g = span.enter();
            info!("Listener actor started");
        }

        let client_id = state.client_id.clone();
        let reader = state.reader.take().ok_or_else(|| {
            ActorProcessingErr::from(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Reader already taken",
            ))
        })?;

        // Read loop: NO long-lived span. We create per-frame spans instead.
        let handle = tokio::spawn({
            let myself = myself.clone();
            async move {
                let mut buf_reader = tokio::io::BufReader::new(reader);

                loop {
                    // Length header is u32 BE.
                    let len = match buf_reader.read_u32().await {
                        Ok(v) => v as usize,
                        Err(e) => {
                            // This is the normal exit path on remote close too.
                            let reason = match e.kind() {
                                std::io::ErrorKind::UnexpectedEof => DisconnectReason::RemoteClosed,
                                _ => DisconnectReason::TransportError(e.to_string()),
                            };

                            let _ = myself.send_message(BrokerMessage::DisconnectRequest {
                                reason,
                                client_id: client_id.clone(),
                                registration_id: None,
                                trace_ctx: None,
                            });
                            return;
                        }
                    };

                    let frame_span = trace_span!(
                        "cassini.listener.read_frame",
                        client_id = %client_id,
                        frame_len = len
                    );
                    let _g = frame_span.enter();

                    if len == 0 {
                        // Treat as keepalive/no-op and avoid log spam.
                        let n = ZERO_LEN_COUNT.fetch_add(1, Ordering::Relaxed) + 1;

                        // Log at most once every 5 seconds with a counter.
                        let t = now_ms();
                        let last = ZERO_LEN_LAST_LOG_MS.load(Ordering::Relaxed);
                        if t.saturating_sub(last) > 5000 {
                            ZERO_LEN_LAST_LOG_MS.store(t, Ordering::Relaxed);
                            debug!(client_id = %client_id, zero_len_seen = n, "Zero-length frames received (suppressed)");
                        }

                        continue;
                    }

                    let mut buffer = vec![0u8; len];
                    if let Err(e) = buf_reader.read_exact(&mut buffer).await {
                        warn!(error = %e, "Failed reading frame body");
                        let _ = myself.send_message(BrokerMessage::DisconnectRequest {
                            reason: DisconnectReason::TransportError(e.to_string()),
                            client_id: client_id.clone(),
                            registration_id: None,
                            trace_ctx: None,
                        });
                        return;
                    }

                    match rkyv::access::<ArchivedClientMessage, Error>(&buffer[..]) {
                        Ok(archived) => match deserialize::<ClientMessage, Error>(archived) {
                            Ok(deserialized) => {
                                trace!(msg = ?deserialized, "Decoded client message");

                                // Convert to broker message and forward to actor handler.
                                let converted = BrokerMessage::from_client_message(
                                    deserialized,
                                    client_id.clone(),
                                );

                                if let Err(e) = myself.send_message(converted) {
                                    warn!(error = %e, "Failed forwarding decoded message to listener actor");
                                }
                            }
                            Err(e) => warn!(error = ?e, "Failed to deserialize archived message"),
                        },
                        Err(e) => warn!(error = ?e, "Failed to parse archived message"),
                    }
                }
            }
        });

        state.task_handle = Some(handle);
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::SetListenerSessionRef { session_ref } => {
                state.session_ref = Some(session_ref);
            }
            BrokerMessage::RegistrationRequest { registration_id, client_id, trace_ctx, } => {
                self.handle_registration_request(myself, state, registration_id, client_id, trace_ctx).await?;
            }
            BrokerMessage::RegistrationResponse { client_id, result, trace_ctx, } => {
                self.handle_registration_response(myself, state, client_id, result, trace_ctx).await?;
            }
            BrokerMessage::DisconnectRequest { reason, client_id, registration_id, trace_ctx } => {
                self.handle_disconnect_request(myself, state, reason, client_id, registration_id, trace_ctx).await?;
            }
            // When the manager tells us to stop (final phase), we just stop.
            BrokerMessage::PrepareForShutdown { .. } => {
                self.handle_prepare_for_shutdown(myself, state).await?;
            }
            BrokerMessage::PublishRequest { topic, payload, registration_id, reply_to, trace_ctx, } => {
                self.handle_publish_request(myself, state, topic, payload, registration_id, reply_to, trace_ctx).await?;
            }
            BrokerMessage::PublishResponse { topic, payload, trace_ctx, result, } => {
                self.handle_publish_response(myself, state, topic, payload, trace_ctx, result).await?;
            }
            BrokerMessage::PublishRequestAck { topic, trace_ctx } => {
                self.handle_publish_request_ack(myself, state, topic, trace_ctx).await?;
            }
            BrokerMessage::SubscribeAcknowledgment { registration_id, topic, trace_ctx, result, } => {
                self.handle_subscribe_ack(myself, state, registration_id, topic, trace_ctx, result).await?;
            }
            BrokerMessage::SubscribeRequest { registration_id, topic, trace_ctx, } => {
                self.handle_subscribe_request(myself, state, registration_id, topic, trace_ctx).await?;
            }
            BrokerMessage::UnsubscribeRequest { registration_id, topic, trace_ctx, } => {
                self.handle_unsubscribe_request(myself, state, registration_id, topic, trace_ctx).await?;
            }
            BrokerMessage::UnsubscribeAcknowledgment { registration_id: _, topic, result, } => {
                self.handle_unsubscribe_ack(myself, state, topic, result).await?;
            }
            BrokerMessage::ControlRequest { registration_id, op, trace_ctx, .. } => {
                self.handle_control_request(myself, state, registration_id, op, trace_ctx).await?;
            }
            BrokerMessage::ControlResponse { registration_id, result, trace_ctx, } => {
                self.handle_control_response(myself, state, registration_id, result, trace_ctx).await?;
            }
            other => {
                warn!(?other, "{UNEXPECTED_MESSAGE_STR}");
            }
        }

        Ok(())
    }
}

impl Listener {
    // ===== Handler methods for Listener =====
    async fn handle_registration_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        registration_id: Option<String>,
        client_id: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.request.registration", %client_id, registration_id = ?registration_id);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("Listener received registration request");

        let broker = state.broker.clone();
        if let Err(e) = broker.send_message(BrokerMessage::RegistrationRequest {
            registration_id,
            client_id: client_id.clone(),
            trace_ctx: Some(span.context()),
        }) {
            let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {e}");
            error!("{err_msg}");

            let msg = ClientMessage::RegistrationResponse {
                result: Err(err_msg.clone()),
                trace_ctx: WireTraceCtx::from_current_span(),
            };

            if let Err(e) = Listener::write(client_id.clone(), msg, Arc::clone(&state.writer)).await {
                error!(error = ?e, "Failed to write RegistrationResponse");
            }

            myself.stop(Some(err_msg));
        } // Note: the else branch for broker not found is removed because broker is guaranteed
        Ok(())
    }

    async fn handle_registration_response(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        client_id: String,
        result: Result<String, String>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.listener.registration_response", %client_id);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("Listener received registration response");

        match result {
            Ok(registration_id) => {
                let reg_id_clone = registration_id.clone();
                state.registration_id = Some(registration_id.clone());

                let msg = ClientMessage::RegistrationResponse {
                    result: Ok(registration_id.clone()),
                    trace_ctx: WireTraceCtx::from_current_span(),
                };

                if let Err(e) = Listener::write(client_id.clone(), msg, Arc::clone(&state.writer)).await {
                    error!(error = ?e, "Failed to write RegistrationResponse");
                    let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                        client_id: state.client_id.clone(),
                        registration_id: state.registration_id.clone().unwrap_or_default(),
                        error: Some(e.to_string()),
                    });
                } else {
                    let session_mgr = state.session_mgr.clone();
                    let myself_clone = myself.clone();
                    let reg_id_clone = reg_id_clone; // move into closure
                    tokio::spawn(async move {
                        let result = session_mgr
                            .call(
                                |reply_to| BrokerMessage::GetSessionRef {
                                    registration_id: reg_id_clone.clone(),
                                    reply_to,
                                },
                                Some(Duration::from_millis(100)),
                            )
                            .await;
                        match result {
                            Ok(CallResult::Success(Some(session_ref))) => {
                                let _ = myself_clone.send_message(BrokerMessage::SetListenerSessionRef { session_ref });
                                debug!("Cached session ref for listener");
                            }
                            _ => {
                                error!("Failed to get session ref for registration {}", reg_id_clone);
                            }
                        }
                    });
                }
            }
            Err(error) => {
                let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {error}");
                let msg = ClientMessage::RegistrationResponse {
                    result: Err(err_msg.clone()),
                    trace_ctx: WireTraceCtx::from_current_span(),
                };

                if let Err(e) = Listener::write(client_id.clone(), msg, Arc::clone(&state.writer)).await {
                    error!(error = ?e, "Failed to write RegistrationResponse");
                }
            }
        }
        Ok(())
    }

    async fn handle_disconnect_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        reason: DisconnectReason,
        client_id: String,
        registration_id: Option<String>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.listener.disconnect_request", %client_id, registration_id = ?registration_id, reason = ?reason);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();

        // Use the registration_id from the message if present, otherwise stored one
        let effective_reg = registration_id.or_else(|| state.registration_id.clone());
        if let Some(reg) = effective_reg {
            if let Some(manager) = myself.try_get_supervisor() {
                let _ = manager.send_message(BrokerMessage::DisconnectRequest {
                    reason: reason.clone(),
                    client_id: client_id.clone(),
                    registration_id: Some(reg),
                    trace_ctx: Some(span.context()),
                });
            }
        }

        debug!("Listener received disconnect request, stopping");
        myself.stop(Some(format!("Disconnect: {:?}", reason)));
        Ok(())
    }

    async fn handle_prepare_for_shutdown(
        &self,
        myself: ActorRef<BrokerMessage>,
        _state: &mut ListenerState,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Listener received shutdown notification, stopping");
        myself.stop(Some("SHUTDOWN_GRACEFUL".to_string()));
        Ok(())
    }

    async fn handle_publish_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        topic: String,
        payload: Arc<Vec<u8>>,
        registration_id: String,
        _reply_to: Option<ActorRef<BrokerMessage>>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = info_span!("cassini.request.publish_request", client_id = %state.client_id, %registration_id, topic = ?topic, payload_bytes = payload.len());
        try_set_parent_otel(&span, trace_ctx.clone());
        let _g = span.enter();
        trace!("Listener received publish request");

        let Some(listener_reg) = state.registration_id.clone() else {
            let err_msg = format!("Bad request: no active session for {registration_id:?}");
            warn!("{err_msg}");
            let _ = Listener::write(
                state.client_id.clone(),
                ClientMessage::ErrorMessage { 
                    error: err_msg, 
                    trace_ctx: WireTraceCtx::from_current_span() 
                },
                Arc::clone(&state.writer),
            )
            .await;
            return Ok(());
        };

        if registration_id != listener_reg {
            let err_msg = format!("Bad request: session mismatch: {registration_id:?}");
            warn!("{err_msg}");
            let _ = Listener::write(
                state.client_id.clone(),
                ClientMessage::ErrorMessage { 
                    error: err_msg, 
                    trace_ctx: WireTraceCtx::from_current_span() 
                },
                Arc::clone(&state.writer),
            )
            .await;
            return Ok(());
        }

        let session = match state.session_ref.as_ref() {
            Some(s) => s.clone(),
            None => {
                error!("No session ref for listener {} – dropping publish", state.client_id);
                return Ok(());
            }
        };

        if let Err(e) = session.send_message(BrokerMessage::PublishRequest {
            registration_id: registration_id.clone(),
            topic: topic.clone(),
            payload: payload.clone(),
            reply_to: None,
            trace_ctx,
        }) {
            let msg = ClientMessage::PublishResponse {
                topic,
                payload,
                result: Err(format!("{PUBLISH_REQ_FAILED_TXT}: {e}")),
                trace_ctx: WireTraceCtx::from_current_span(),
            };
            if let Err(e) = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await {
                error!(error = ?e, "Failed to write PublishResponse");
            }
        }
        Ok(())
    }

    async fn handle_publish_response(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        topic: String,
        payload: Arc<Vec<u8>>,
        trace_ctx: Option<opentelemetry::Context>,
        result: Result<(), String>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.listener.publish_response", topic = ?topic, ok = result.is_ok());
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("Listener received publish response");

        let msg = ClientMessage::PublishResponse { 
            topic, 
            payload, 
            result,
            trace_ctx: WireTraceCtx::from_current_span(),
        };
        if let Err(e) = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await {
            error!(error = ?e, "Failed to write PublishResponse");
        }
        Ok(())
    }

    async fn handle_publish_request_ack(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.listener.publish_ack", topic = ?topic);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();

        let msg = ClientMessage::PublishRequestAck {
            topic,
            trace_ctx: WireTraceCtx::from_current_span(),
        };
        if let Err(e) = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await {
            error!(error = ?e, "Failed to write PublishRequestAck");
        }
        Ok(())
    }

    async fn handle_subscribe_ack(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
        result: Result<(), String>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.listener.subscribe_ack", %registration_id, topic = ?topic, ok = result.is_ok());
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();

        let msg = ClientMessage::SubscribeAcknowledgment { 
            topic, 
            result,
            trace_ctx: WireTraceCtx::from_current_span(),
        };
        if let Err(e) = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await {
            error!(error = ?e, "Failed to write SubscribeAcknowledgment");
        }
        Ok(())
    }

    async fn handle_subscribe_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.request.subscribe", client_id = %state.client_id, %registration_id, topic = ?topic);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("Listener received subscribe request");

        let Some(listener_reg) = state.registration_id.clone() else {
            warn!("Subscribe from unregistered client");
            let msg = ClientMessage::SubscribeAcknowledgment {
                topic,
                result: Err("Bad request: not registered".to_string()),
                trace_ctx: WireTraceCtx::from_current_span(),
            };
            let _ = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await;
            return Ok(());
        };

        if registration_id != listener_reg {
            warn!("Subscribe session mismatch");
            let msg = ClientMessage::SubscribeAcknowledgment {
                topic,
                result: Err("Bad request: session mismatch".to_string()),
                trace_ctx: WireTraceCtx::from_current_span(),
            };
            let _ = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await;
            return Ok(());
        }

        match where_is(listener_reg.clone()) {
            Some(session) => {
                if let Err(e) = session.send_message(BrokerMessage::SubscribeRequest {
                    registration_id: listener_reg,
                    topic,
                    trace_ctx: Some(span.context()),
                }) {
                    let err_msg = format!(
                        "{SUBSCRIBE_REQUEST_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: {e}"
                    );
                    error!("{err_msg}");
                    let _ = Listener::write(
                        state.client_id.clone(),
                        ClientMessage::ErrorMessage { 
                            error: err_msg, 
                            trace_ctx: WireTraceCtx::from_current_span() 
                        },
                        Arc::clone(&state.writer),
                    )
                    .await;
                }
            }
            None => {
                error!("Could not forward subscribe to session; closing connection");
                let msg = ClientMessage::SubscribeAcknowledgment {
                    topic,
                    result: Err("Failed to complete request: session missing".to_string()),
                    trace_ctx: WireTraceCtx::from_current_span(),
                };
                let _ = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await;
                myself.stop(Some(SESSION_MISSING_REASON_STR.to_string()));
            }
        }
        Ok(())
    }

    async fn handle_unsubscribe_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.request.unsubscribe", client_id = %state.client_id, %registration_id, topic = ?topic);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("Listener received unsubscribe request");

        let Some(listener_reg) = state.registration_id.clone() else {
            warn!("Unsubscribe from unregistered client");
            return Ok(());
        };

        if registration_id != listener_reg {
            warn!("Unsubscribe session mismatch");
            return Ok(());
        }

        match where_is(registration_id.clone()) {
            Some(session) => {
                if let Err(e) = session.send_message(BrokerMessage::UnsubscribeRequest {
                    registration_id,
                    topic,
                    trace_ctx: Some(span.context()),
                }) {
                    let err_msg = format!(
                        "{SUBSCRIBE_REQUEST_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: {e}"
                    );
                    error!("{err_msg}");
                    let _ = Listener::write(
                        state.client_id.clone(),
                        ClientMessage::ErrorMessage { 
                            error: err_msg, 
                            trace_ctx: WireTraceCtx::from_current_span() 
                        },
                        Arc::clone(&state.writer),
                    )
                    .await;
                }
            }
            None => {
                let err_msg = format!("{SUBSCRIBE_REQUEST_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
                error!("{err_msg}");
                let _ = Listener::write(
                    state.client_id.clone(),
                    ClientMessage::ErrorMessage { 
                        error: err_msg, 
                        trace_ctx: WireTraceCtx::from_current_span() 
                    },
                    Arc::clone(&state.writer),
                )
                .await;
            }
        }
        Ok(())
    }

    async fn handle_unsubscribe_ack(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        topic: String,
        result: Result<(), String>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.listener.unsubscribe_ack", topic = ?topic);
        let _g = span.enter();

        debug!(topic = ?topic, "Unsubscribe acknowledged");
        let msg = ClientMessage::UnsubscribeAcknowledgment {
            topic,
            result,
            trace_ctx: WireTraceCtx::from_current_span(),
        };
        let _ = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await;
        Ok(())
    }

    async fn handle_control_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        registration_id: String,
        op: ControlOp,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.request.control", client_id = %state.client_id, %registration_id, op = ?op);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("Listener received control request");

        let Some(listener_reg) = state.registration_id.clone() else {
            warn!("Control from unregistered client");
            return Ok(());
        };

        if registration_id != listener_reg {
            warn!("Control session mismatch");
            return Ok(());
        }

        if let Some(session) = where_is(listener_reg.clone()) {
            let _ = session.send_message(BrokerMessage::ControlRequest {
                registration_id: listener_reg,
                op,
                reply_to: None,
                trace_ctx: Some(span.context()),
            });
        }
        Ok(())
    }

    async fn handle_control_response(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        registration_id: String,
        result: Result<ControlResult, ControlError>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.listener.control_response", %registration_id, ok = result.is_ok());
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("Received ControlResponse");

        let msg = ClientMessage::ControlResponse {
            registration_id,
            result,
            trace_ctx: WireTraceCtx::from_current_span(),
        };

        if let Err(e) = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await {
            error!(error = ?e, "Failed to write ControlResponse");
        }
        Ok(())
    }
}
