use crate::UNEXPECTED_MESSAGE_STR;
use crate::{
    BROKER_NAME, BROKER_NOT_FOUND_TXT, PUBLISH_REQ_FAILED_TXT, REGISTRATION_REQ_FAILED_TXT,
    SESSION_MISSING_REASON_STR, SESSION_NOT_FOUND_TXT, SUBSCRIBE_REQUEST_FAILED_TXT,
};
use async_trait::async_trait;
use cassini_types::{ArchivedClientMessage, BrokerMessage, ClientMessage, DisconnectReason};
use opentelemetry::Context;
use ractor::{registry::where_is, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
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
}

pub struct ListenerManagerArgs {
    pub bind_addr: String,
    pub server_cert_file: String,
    pub private_key_file: String,
    pub ca_cert_file: String,
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

        // Accept loop runs in its own task; do NOT keep an entered span alive forever.
        let _ = tokio::spawn({
            let myself = myself.clone();
            async move {
                loop {
                    let (stream, peer_addr) = match server.accept().await {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(error = %e, "TCP accept failed");
                            continue;
                        }
                    };

                    let handshake_span = info_span!(
                        "cassini.listener_manager.accept",
                        peer_addr = %peer_addr
                    );

                    let acceptor = acceptor.clone();
                    let myself = myself.clone();

                    tokio::spawn(async move {
                        let _g = handshake_span.enter();

                        let stream = match acceptor.accept(stream).await {
                            Ok(s) => s,
                            Err(e) => {
                                warn!(error = %e, "TLS handshake failed");
                                return;
                            }
                        };

                        let client_id = uuid::Uuid::new_v4().to_string();
                        let (reader, writer) = split(stream);
                        let writer = BufWriter::new(writer);

                        let listener_args = ListenerArguments {
                            writer: Arc::new(Mutex::new(writer)),
                            reader: Some(reader),
                            client_id: client_id.clone(),
                            registration_id: None,
                        };

                        let spawn_span = info_span!(
                            "cassini.listener_manager.spawn_listener",
                            client_id = %client_id
                        );
                        let _g2 = spawn_span.enter();

                        if let Err(e) = Actor::spawn_linked(
                            Some(client_id),
                            Listener,
                            listener_args,
                            myself.clone().into(),
                        )
                        .await
                        {
                            error!(error = ?e, "Failed to spawn Listener actor for connection");
                        }
                    });
                }
            }
        });

        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(actor_cell) => {
                debug!(
                    worker_name = ?actor_cell.get_name(),
                    worker_id = ?actor_cell.get_id(),
                    "Worker started"
                );
            }
            _ => (),
        }
        Ok(())
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::RegistrationResponse {
                client_id,
                result,
                trace_ctx,
            } => {
                let span = trace_span!("cassini.listener_manager.registration_response", %client_id);
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _g = span.enter();

                trace!("Forwarding registration response to listener");

                match where_is(client_id.clone()) {
                    Some(listener) => {
                        if let Err(e) = listener.send_message(BrokerMessage::RegistrationResponse {
                            client_id,
                            result,
                            trace_ctx: Some(span.context()),
                        }) {
                            error!(error = %e, "Failed to forward RegistrationResponse to listener");
                        }
                    }
                    None => {
                        warn!("Couldn't find listener {client_id}");
                    }
                }
            }

            BrokerMessage::DisconnectRequest {
                reason,
                client_id,
                registration_id,
                trace_ctx,
            } => {
                let span = trace_span!(
                    "cassini.listener_manager.disconnect_request",
                    %client_id,
                    registration_id = ?registration_id,
                    reason = ?reason
                );
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _g = span.enter();

                trace!("ListenerManager handling disconnect");

                match where_is(client_id.clone()) {
                    Some(listener) => {
                        if let Some(id) = registration_id {
                            match where_is(id.clone()) {
                                Some(session) => match reason {
                                    DisconnectReason::RemoteClosed => {
                                        info!("Client disconnected (remote closed)");
                                        let _ = session.send_message(BrokerMessage::DisconnectRequest {
                                            reason,
                                            client_id: client_id.clone(),
                                            registration_id: Some(id),
                                            trace_ctx: Some(span.context()),
                                        });
                                    }
                                    DisconnectReason::TransportError(err) => {
                                        warn!("Client disconnected unexpectedly; notifying session");
                                        let _ = session.send_message(BrokerMessage::TimeoutMessage {
                                            client_id: client_id.clone(),
                                            registration_id: id,
                                            error: Some(err),
                                        });
                                    }
                                },
                                None => warn!("{SESSION_NOT_FOUND_TXT}: {id}"),
                            }
                        }
                        listener.stop(None);
                    }
                    None => warn!("Couldn't find listener {client_id}"),
                }
            }

            _ => (),
        }

        Ok(())
    }
}

// ============================== Listener actor ============================== //

struct Listener;

struct ListenerState {
    writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    client_id: String,
    registration_id: Option<String>,
    task_handle: Option<JoinHandle<()>>,
}

struct ListenerArguments {
    writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    client_id: String,
    registration_id: Option<String>,
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

        // CAUTION: keep this u32. The reader expects u32.
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
            registration_id: args.registration_id,
            task_handle: None,
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
            BrokerMessage::RegistrationRequest {
                registration_id,
                client_id,
                ..
            } => {
                // Fresh root per inbound request so Jaeger shows broker/session spans clearly.
                let span = trace_span!("cassini.request.registration", %client_id, registration_id = ?registration_id);
                let _ = span.set_parent(Context::new()); // break any ambient parent
                let _g = span.enter();

                trace!("Listener received registration request");

                match where_is(BROKER_NAME.to_string()) {
                    Some(broker) => {
                        if let Err(e) = broker.send_message(BrokerMessage::RegistrationRequest {
                            registration_id,
                            client_id: client_id.clone(),
                            trace_ctx: Some(span.context()),
                        }) {
                            let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {e}");
                            error!("{err_msg}");

                            let msg = ClientMessage::RegistrationResponse {
                                result: Err(err_msg.clone()),
                            };

                            if let Err(e) = Listener::write(client_id.clone(), msg, Arc::clone(&state.writer)).await {
                                error!(error = ?e, "Failed to write RegistrationResponse");
                            }

                            myself.stop(Some(err_msg));
                        }
                    }
                    None => {
                        let err_msg =
                            format!("{REGISTRATION_REQ_FAILED_TXT}: {BROKER_NOT_FOUND_TXT}");
                        error!("{err_msg}");

                        let msg = ClientMessage::RegistrationResponse {
                            result: Err(err_msg.clone()),
                        };

                        if let Err(e) = Listener::write(client_id.clone(), msg, Arc::clone(&state.writer)).await {
                            error!(error = ?e, "Failed to write RegistrationResponse");
                        }

                        myself.stop(Some(err_msg));
                    }
                }
            }

            BrokerMessage::RegistrationResponse {
                client_id,
                result,
                trace_ctx,
            } => {
                let span = trace_span!("cassini.listener.registration_response", %client_id);
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _g = span.enter();

                trace!("Listener received registration response");

                match result {
                    Ok(registration_id) => {
                        state.registration_id = Some(registration_id.clone());

                        let msg = ClientMessage::RegistrationResponse {
                            result: Ok(registration_id),
                        };

                        if let Err(e) = Listener::write(client_id.clone(), msg, Arc::clone(&state.writer)).await {
                            error!(error = ?e, "Failed to write RegistrationResponse");
                            let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                                client_id: state.client_id.clone(),
                                registration_id: state.registration_id.clone().unwrap_or_default(),
                                error: Some(e.to_string()),
                            });
                        }
                    }
                    Err(error) => {
                        let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {error}");
                        let msg = ClientMessage::RegistrationResponse {
                            result: Err(err_msg.clone()),
                        };

                        if let Err(e) = Listener::write(client_id.clone(), msg, Arc::clone(&state.writer)).await {
                            error!(error = ?e, "Failed to write RegistrationResponse");
                        }
                    }
                }
            }

            BrokerMessage::PublishRequest {
                topic,
                payload,
                registration_id,
                ..
            } => {
                let span = trace_span!(
                    "cassini.request.publish",
                    client_id = %state.client_id,
                    %registration_id,
                    topic = ?topic,
                    payload_bytes = payload.len()
                );
                let _ = span.set_parent(Context::new());
                let otel_ctx = span.context();
                let _g = span.enter();

                trace!("Listener received publish request");

                let Some(listener_reg) = state.registration_id.clone() else {
                    let err_msg = format!("Bad request: no active session for {registration_id:?}");
                    warn!("{err_msg}");
                    let _ = Listener::write(
                        state.client_id.clone(),
                        ClientMessage::ErrorMessage(err_msg),
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
                        ClientMessage::ErrorMessage(err_msg),
                        Arc::clone(&state.writer),
                    )
                    .await;
                    return Ok(());
                }

                match where_is(registration_id.clone()) {
                    Some(session) => {
                        if let Err(e) = session.send_message(BrokerMessage::PublishRequest {
                            registration_id: registration_id.clone(),
                            topic: topic.clone(),
                            payload: payload.clone(),
                            trace_ctx: Some(otel_ctx),
                        }) {
                            let msg = ClientMessage::PublishResponse {
                                topic,
                                payload,
                                result: Err(format!(
                                    "{PUBLISH_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: {e}"
                                )),
                            };
                            if let Err(e) = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await {
                                error!(error = ?e, "Failed to write PublishResponse");
                            }
                        }
                    }
                    None => {
                        let msg = ClientMessage::PublishResponse {
                            topic,
                            payload,
                            result: Err(format!("{PUBLISH_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}")),
                        };
                        let _ = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await;
                    }
                }
            }

            BrokerMessage::PublishResponse {
                topic,
                payload,
                trace_ctx,
                result,
            } => {
                let span = trace_span!("cassini.listener.publish_response", topic = ?topic, ok = result.is_ok());
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _g = span.enter();

                trace!("Listener received publish response");

                let msg = ClientMessage::PublishResponse { topic, payload, result };
                if let Err(e) = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await {
                    error!(error = ?e, "Failed to write PublishResponse");
                }
            }

            BrokerMessage::PublishRequestAck { topic, trace_ctx } => {
                let span = trace_span!("cassini.listener.publish_ack", topic = ?topic);
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _g = span.enter();

                let msg = ClientMessage::PublishRequestAck(topic);
                if let Err(e) = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await {
                    error!(error = ?e, "Failed to write PublishRequestAck");
                }
            }

            BrokerMessage::SubscribeAcknowledgment {
                registration_id,
                topic,
                trace_ctx,
                result,
            } => {
                let span = trace_span!("cassini.listener.subscribe_ack", %registration_id, topic = ?topic, ok = result.is_ok());
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _g = span.enter();

                let msg = ClientMessage::SubscribeAcknowledgment { topic, result };
                if let Err(e) = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await {
                    error!(error = ?e, "Failed to write SubscribeAcknowledgment");
                }
            }

            BrokerMessage::SubscribeRequest {
                registration_id,
                topic,
                ..
            } => {
                let span = trace_span!("cassini.request.subscribe", client_id = %state.client_id, %registration_id, topic = ?topic);
                let _ = span.set_parent(Context::new());
                let _g = span.enter();

                trace!("Listener received subscribe request");

                let Some(listener_reg) = state.registration_id.clone() else {
                    warn!("Subscribe from unregistered client");
                    let msg = ClientMessage::SubscribeAcknowledgment {
                        topic,
                        result: Err("Bad request: not registered".to_string()),
                    };
                    let _ = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await;
                    return Ok(());
                };

                if registration_id != listener_reg {
                    warn!("Subscribe session mismatch");
                    let msg = ClientMessage::SubscribeAcknowledgment {
                        topic,
                        result: Err("Bad request: session mismatch".to_string()),
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
                                ClientMessage::ErrorMessage(err_msg),
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
                        };
                        let _ = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await;
                        myself.stop(Some(SESSION_MISSING_REASON_STR.to_string()));
                    }
                }
            }

            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                ..
            } => {
                let span = trace_span!("cassini.request.unsubscribe", client_id = %state.client_id, %registration_id, topic = ?topic);
                let _ = span.set_parent(Context::new());
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
                                ClientMessage::ErrorMessage(err_msg),
                                Arc::clone(&state.writer),
                            )
                            .await;
                        }
                    }
                    None => {
                        let err_msg =
                            format!("{SUBSCRIBE_REQUEST_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
                        error!("{err_msg}");
                        let _ = Listener::write(
                            state.client_id.clone(),
                            ClientMessage::ErrorMessage(err_msg),
                            Arc::clone(&state.writer),
                        )
                        .await;
                    }
                }
            }

            BrokerMessage::UnsubscribeAcknowledgment {
                registration_id: _,
                topic,
                ..
            } => {
                debug!(topic = ?topic, "Unsubscribe acknowledged");
                let msg = ClientMessage::UnsubscribeAcknowledgment {
                    topic,
                    result: Ok(()),
                };
                let _ = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await;
            }

            BrokerMessage::DisconnectRequest { reason, client_id, .. } => {
                let span = trace_span!("cassini.request.disconnect", %client_id, reason = ?reason);
                let _ = span.set_parent(Context::new());
                let _g = span.enter();

                trace!("Listener received disconnect request");

                if let Some(manager) = myself.try_get_supervisor() {
                    let _ = manager.send_message(BrokerMessage::DisconnectRequest {
                        reason,
                        client_id,
                        registration_id: state.registration_id.clone(),
                        trace_ctx: Some(span.context()),
                    });
                } else {
                    error!("Failed to locate ListenerManager; killing listener");
                    myself.kill();
                }
            }

            BrokerMessage::ControlRequest {
                registration_id,
                op,
                ..
            } => {
                let span = trace_span!("cassini.request.control", client_id = %state.client_id, %registration_id, op = ?op);
                let _ = span.set_parent(Context::new());
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
            }

            BrokerMessage::ControlResponse {
                registration_id,
                result,
                trace_ctx,
            } => {
                let span = trace_span!("cassini.listener.control_response", %registration_id, ok = result.is_ok());
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _g = span.enter();

                trace!("Received ControlResponse");

                let msg = ClientMessage::ControlResponse {
                    registration_id,
                    result,
                };

                if let Err(e) = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await {
                    error!(error = ?e, "Failed to write ControlResponse");
                }
            }

            other => {
                warn!(?other, "{UNEXPECTED_MESSAGE_STR}");
            }
        }

        Ok(())
    }
}
