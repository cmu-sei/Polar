use cassini_types::{ClientEvent, ClientMessage, ControlOp, WireTraceCtx};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort, Message};
use ractor::concurrency::{sleep, Duration};
use rkyv::rancor::Error;
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::ClientConfig;
use std::env;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::io::{split, AsyncReadExt, BufWriter, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::task::AbortHandle;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tracing::{debug, debug_span, trace_span, warn_span, error, info, info_span, trace, warn, Instrument};
use std::marker::PhantomData;
pub use cassini_tracing::{init_tracing, shutdown_tracing};
pub use cassini_tracing::{try_set_parent_otel, try_set_parent_wire};
use socket2::SockRef;

pub const UNEXPECTED_DISCONNECT: &str = "UNEXPECTED_DISCONNECT";
pub const REGISTRATION_EXPECTED: &str =
    "Expected client to be registered to conduct this operation.";

pub const CLIENT_DISCONNECTED: &str = "CLIENT_DISCONNECTED";

/// Bridges [`ClientEvent`] messages from the TCP client to any actor using a mapping closure.
///
/// This is the recommended way to receive events from [`TcpClientActor`]. Unlike subscribing
/// to the [`OutputPort`], this uses ractor's normal mpsc mailbox and will never silently drop
/// messages under load. See: https://github.com/slawlor/ractor/issues/225
pub struct ClientEventForwarder<M: Message + Sync>(PhantomData<M>);

impl<M: Message + Sync> ClientEventForwarder<M> {
    pub fn new() -> Self {
        ClientEventForwarder(PhantomData)
    }
}

pub struct ClientEventForwarderArgs<M: Message + Sync> {
    pub target: ActorRef<M>,
    pub mapper: Box<dyn Fn(ClientEvent) -> Option<M> + Send + Sync + 'static>,
}

pub struct ClientEventForwarderState<M: Message + Sync> {
    target: ActorRef<M>,
    mapper: Box<dyn Fn(ClientEvent) -> Option<M> + Send + Sync + 'static>,
}

#[async_trait]
impl<M: Message + Sync> Actor for ClientEventForwarder<M> {
    type Msg = ClientEvent;
    type State = ClientEventForwarderState<M>;
    type Arguments = ClientEventForwarderArgs<M>;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(ClientEventForwarderState {
            target: args.target,
            mapper: args.mapper,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let Some(mapped) = (state.mapper)(msg) {
            if let Err(e) = state.target.send_message(mapped) {
                warn!("ClientEventForwarder: target actor unreachable: {e}");
            }
        }
        Ok(())
    }
}

/// A basse configuration for a TCP Client actor
pub struct TCPClientConfig {
    pub broker_endpoint: String,
    pub server_name: String,
    pub ca_certificate_path: String,
    pub client_certificate_path: String,
    pub client_key_path: String,
}

#[derive(Debug)]
pub enum RegistrationState {
    Unregistered,
    Registered { registration_id: String },
}

impl TCPClientConfig {
    /// Read filepaths from the environment and return. If we can't read these, we can't start
    pub fn new() -> Result<Self, ActorProcessingErr> {
        let client_certificate_path = env::var("TLS_CLIENT_CERT")?;
        let client_key_path = env::var("TLS_CLIENT_KEY")?;
        let ca_certificate_path = env::var("TLS_CA_CERT")?;
        let broker_endpoint = env::var("BROKER_ADDR")?;
        let server_name = env::var("CASSINI_SERVER_NAME")?;

        Ok(TCPClientConfig {
            broker_endpoint,
            server_name,
            ca_certificate_path,
            client_certificate_path,
            client_key_path,
        })
    }
}

/// Messages handled by the TCP client actor
#[derive(Debug)]
pub enum TcpClientMessage {
    // Send(ClientMessage),
    RegistrationResponse(String),
    ErrorMessage(String),
    /// Returns the registration id when queried, unlikely to be useful
    /// unless some other agent dies before the client does and needs to get it back
    GetRegistrationId(RpcReplyPort<Option<String>>),
    /// Message that gets emitted when the client successfully registers with the broker.
    /// It should contain a valid registration uid
    ClientRegistered(String),
    // attempt to register with the broker, optionally with a registration id to restart a session.
    Register,
    /// Publish request from the client.
    Publish {
        topic: String,
        payload: Vec<u8>,
        trace_ctx: Option<WireTraceCtx>,
    },
    Subscribe {
        topic: String,
        trace_ctx: Option<WireTraceCtx>,
    },
    /// Unsubscribe request from the client.
    /// Contains the topic
    UnsubscribeRequest {
        topic: String,
        trace_ctx: Option<WireTraceCtx>,
    },
    ///Disconnect, sending a session id to end, if any
    Disconnect {
        trace_ctx: Option<WireTraceCtx>,
    },
    /// Control plane operations
    ControlRequest {
        op: cassini_types::ControlOp,
        trace_ctx: Option<WireTraceCtx>,
    },
    ListSessions {
        trace_ctx: Option<WireTraceCtx>,
    },
    ListTopics {
        trace_ctx: Option<WireTraceCtx>,
    },
    GetSession {
        trace_ctx: Option<WireTraceCtx>,
    },
    SetEventHandler(ActorRef<ClientEvent>),
    Reconnect,
    Reconnected {
        reader: Option<ReadHalf<TlsStream<TcpStream>>>,
        writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    },
}

/// Actor state for the TCP client
pub struct TcpClientState {
    bind_addr: String,
    server_name: String,
    writer: Option<Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>, // Use Option to allow taking ownership
    registration: RegistrationState,
    client_config: Arc<ClientConfig>,

    /// Output port where message payloads and the topic it came from is piped to a dispatcher
    events_output: Arc<OutputPort<ClientEvent>>,
    abort_handle: Option<AbortHandle>, // abort handle switch for the stream read loop
    pub event_handler: Option<ActorRef<ClientEvent>>,
}

pub struct TcpClientArgs {
    pub config: TCPClientConfig,
    pub registration_id: Option<String>,
    /// If `None`, a default unsubscribed port is created internally.
    /// Prefer [`event_handler`] via [`ClientEventForwarder`] for reliable delivery.
    pub events_output: Option<Arc<OutputPort<ClientEvent>>>,
    pub event_handler: Option<ActorRef<ClientEvent>>,
}

/// TCP client actor
pub struct TcpClientActor;

impl TcpClientActor {
    fn current_trace_ctx() -> Option<WireTraceCtx> {
        // Use the helper method from WireTraceCtx itself
        WireTraceCtx::from_current_span()
    }
    
    #[tracing::instrument(name="cassini.tcp_client.connect_with_backoff", skip(client_config))]
    async fn connect_with_backoff(
        addr: String,
        server_name: String,
        client_config: &Arc<ClientConfig>,
    ) -> Result<TlsStream<TcpStream>, ActorProcessingErr> {
        let connector = TlsConnector::from(Arc::clone(client_config));
        let mut backoff = std::time::Duration::from_millis(100);
        let max_backoff = std::time::Duration::from_secs(2);
        let max_attempts = 8;

        let server_name = ServerName::try_from(server_name)
            .map_err(|_| ActorProcessingErr::from("Invalid DNS name"))?;

        for attempt in 1..=max_attempts {
            match TcpStream::connect(addr.clone()).await {
                Ok(tcp) => {
                    // Convert to std::net::TcpStream to set socket options
                    let std_stream = tcp.into_std().map_err(|e| {
                        ActorProcessingErr::from(format!("Failed to convert to std stream: {}", e))
                    })?;

                    // Use socket2 to set buffer sizes
                    let sock_ref = SockRef::from(&std_stream);
                    sock_ref.set_recv_buffer_size(100 * 1024 * 1024).map_err(|e| {
                        ActorProcessingErr::from(format!("Failed to set recv buffer size: {}", e))
                    })?;
                    sock_ref.set_send_buffer_size(100 * 1024 * 1024).map_err(|e| {
                        ActorProcessingErr::from(format!("Failed to set send buffer size: {}", e))
                    })?;

                    // Convert back to tokio stream
                    let tcp = tokio::net::TcpStream::from_std(std_stream).map_err(|e| {
                        ActorProcessingErr::from(format!("Failed to convert back to tokio stream: {}", e))
                    })?;

                    match connector.connect(server_name.clone(), tcp).await {
                        Ok(tls) => return Ok(tls),
                        Err(e) => debug!(attempt, error = %e, "TLS handshake failed, retrying"),
                    }
                }
                Err(e) => debug!(attempt, error = %e, "TCP connect failed, retrying"),
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(max_backoff);
        }
        Err(ActorProcessingErr::from("Failed to connect after retries"))
    }

    fn spawn_reader(
        &self,
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
    ) -> Result<(), ActorProcessingErr> {
        // Take ownership of the read half exactly once.
        let reader = state.reader.take().expect("Reader already taken");

        let queue_out = state.events_output.clone();
        let handler = state.event_handler.clone(); // clone the optional direct handler
        let broker_addr = state.bind_addr.clone();

        let read_loop_span = info_span!("cassini.tcp_client.read_loop", broker_addr = %broker_addr,);

        let abort = tokio::spawn(async move {
            let mut msg_count = 0;
            let mut buf_reader = tokio::io::BufReader::new(reader);
            loop {
                msg_count += 1;
                trace!("TCP client read loop iteration {}", msg_count);
                match buf_reader.read_u32().await {
                    Ok(incoming_msg_length) => {
                        if incoming_msg_length > 0 {
                            let mut buffer = vec![0; incoming_msg_length as usize];

                            trace!("Reading {incoming_msg_length} byte(s).");
                            if let Ok(_) = buf_reader.read_exact(&mut buffer).await {
                                let _fg = trace_span!(
                                    "tcp_client.read_frame",
                                    len = incoming_msg_length,
                                    first_16 = ?&buffer[..buffer.len().min(16)],
                                )
                                .entered();

                                trace!(
                                    len = incoming_msg_length,
                                    first_16 = ?&buffer[..buffer.len().min(16)],
                                    "Received frame"
                                );

                                match rkyv::from_bytes::<ClientMessage, Error>(&buffer) {
                                    Ok(message) => {
                                        match message {
                                            ClientMessage::RegistrationResponse { result, trace_ctx } => {
                                                let span = trace_span!("client.handle_registration_response");
                                                try_set_parent_wire(&span, trace_ctx);
                                                let _g = span.enter();

                                                match result {
                                                    Ok(registration_id) => {
                                                        myself
                                                            .send_message(
                                                                TcpClientMessage::RegistrationResponse(
                                                                    registration_id,
                                                                ),
                                                            )
                                                            .expect("Could not forward message to {myself:?");
                                                    }
                                                    Err(e) => {
                                                        warn!("Failed to register session with the server. {e}");
                                                    }
                                                }
                                            }
                                            ClientMessage::PublishResponse {
                                                topic,
                                                payload,
                                                result,
                                                trace_ctx,
                                            } => {
                                                let span = trace_span!("client.handle_publish_response");
                                                if let Some(wire_ctx) = trace_ctx {
                                                    tracing::debug!("Sink TCP client received PublishResponse with trace_id: {:02x?}", wire_ctx.trace_id);
                                                }
                                                try_set_parent_wire(&span, trace_ctx);
                                                let _g = span.enter();

                                                if result.is_ok() {
                                                    let current_trace_ctx = WireTraceCtx::from_current_span();
                                                    let event = ClientEvent::MessagePublished {
                                                        topic: topic.clone(),
                                                        payload: payload.to_vec(),
                                                        trace_ctx: current_trace_ctx,
                                                    };
                                                    // Send to output port (existing)
                                                    queue_out.send(event.clone());
                                                    trace!("TCP client sent MessagePublished to output port for topic {}", topic);
                                                    // Send to direct handler if present
                                                    if let Some(ref handler) = handler {
                                                        if let Err(e) = handler.send_message(event) {
                                                            error!("Failed to send MessagePublished to direct handler: {}", e);
                                                        }
                                                    }
                                                } else {
                                                    warn!("Failed to publish message to topic: {topic}");
                                                }
                                            }
                                            ClientMessage::SubscribeAcknowledgment { topic, result, trace_ctx } => {
                                                let span = trace_span!("client.handle_subscribe_ack");
                                                try_set_parent_wire(&span, trace_ctx);
                                                let _g = span.enter();

                                                if result.is_ok() {
                                                    info!("Successfully subscribed to topic: {topic}");
                                                } else {
                                                    warn!("Failed to subscribe to topic: {topic}, {result:?}");
                                                }
                                            }

                                            ClientMessage::UnsubscribeAcknowledgment { topic, result, trace_ctx } => {
                                                let span = trace_span!("client.handle_unsubscribe_ack");
                                                try_set_parent_wire(&span, trace_ctx);
                                                let _g = span.enter();

                                                if result.is_ok() {
                                                    info!("Successfully unsubscribed from topic: {topic}");
                                                } else {
                                                    warn!("Failed to unsubscribe from topic: {topic}");
                                                }
                                            }
                                            ClientMessage::ErrorMessage { error, trace_ctx } => {
                                                let span = trace_span!("client.handle_error_message").entered();
                                                try_set_parent_wire(&span, trace_ctx);
                                                let _g = span.enter();
                                                warn!("Received error from broker: {error}");
                                            }
                                            ClientMessage::PublishRequestAck { topic, trace_ctx } => {
                                                let span = trace_span!("client.handle_publish_ack");
                                                try_set_parent_wire(&span, trace_ctx);
                                                let _g = span.enter();
                                                trace!("Received publish ack for topic: {topic}");
                                            }
                                            ClientMessage::ControlResponse { registration_id, result, trace_ctx } => {
                                                let span = trace_span!("client.handle_control_response");
                                                try_set_parent_wire(&span, trace_ctx);
                                                let _g = span.enter();

                                                trace!("Received control response for registration: {registration_id}, result: {:?}", result);

                                                let current_trace_ctx = WireTraceCtx::from_current_span();
                                                let event = ClientEvent::ControlResponse {
                                                    registration_id,
                                                    result,
                                                    trace_ctx: current_trace_ctx,
                                                };
                                                // Send to output port
                                                queue_out.send(event.clone());
                                                // Send to direct handler if present
                                                if let Some(ref handler) = handler {
                                                    if let Err(e) = handler.send_message(event) {
                                                        error!("Failed to send ControlResponse to direct handler: {}", e);
                                                    }
                                                }
                                            }
                                            _ => {
                                                let _g = trace_span!("client.handle_unknown_message").entered();
                                                trace!("Received unexpected message type");
                                            },
                                        }
                                    }
                                    Err(e) => {
                                        error!(
                                            len = buffer.len(),
                                            first_8 = ?&buffer[..buffer.len().min(8)],
                                            "{e} Received raw frame"
                                        );
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "TCP reader terminating");
                        let event = ClientEvent::TransportError {
                            reason: format!("{UNEXPECTED_DISCONNECT} {e}")
                        };
                        queue_out.send(event.clone());
                        if let Some(ref handler) = handler {
                            if let Err(e) = handler.send_message(event) {
                                error!("Failed to send TransportError to direct handler: {}", e);
                            }
                        }
                        // Instead of breaking, send Reconnect to self
                        let _ = myself.send_message(TcpClientMessage::Reconnect);
                        break;
                    }
                }
            }
        }.instrument(read_loop_span))
        .abort_handle();

        state.abort_handle = Some(abort);
        Ok(())
    }

    /// Send a payload to the broker over the write.
    pub async fn send_message(
        message: ClientMessage,
        state: &mut TcpClientState,
    ) -> Result<(), ActorProcessingErr> {
        // Get current trace context
        let trace_ctx = WireTraceCtx::from_current_span();
        debug!("Sending message with trace_ctx: {:?}", trace_ctx);

        // NOTE: Broker expects u32 length prefix, NOT u64
        match rkyv::to_bytes::<Error>(&message) {
            Ok(bytes) => {
                // Validate frame size fits in u32
                let len: u32 = bytes.len().try_into().map_err(|_| {
                    ActorProcessingErr::from("Frame too large (>4GB)")
                })?;
                
                // Pre-allocate buffer with exact capacity (4 bytes + payload)
                let mut buffer = Vec::with_capacity(4 + bytes.len());
                
                // Write 4-byte BE length prefix (matching broker's read_u32)
                buffer.extend_from_slice(&len.to_be_bytes());

                //add message to buffer
                buffer.extend_from_slice(&bytes);

                //write message
                if let Some(arc_writer) = state.writer.as_ref() {
                    let mut writer = arc_writer.lock().await;

                    // Write entire buffer in one go
                    writer.write_all(&buffer).await?;

                    // Flush to ensure data is sent immediately (CRITICAL!)
                    writer.flush().await?;
                    Ok(())
                } else {
                    error!("No writer available in TcpClientState");
                    let reason = "No writer assigned to client!".to_string();
                    return Err(ActorProcessingErr::from(reason));
                }
            }
            Err(e) => {
                warn!("Failed to serialize message. {e}");
                Err(ActorProcessingErr::from(e))
            }
        }
    }

    pub fn emit_transport_err_and_stop(
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
        reason: String,
    ) {
        let _g = warn_span!("tcp_client.transport_error", error = %reason).entered();
        state.events_output.send(ClientEvent::TransportError {
            reason: reason.clone(),
        });
        drop(_g);
        myself.stop(Some(reason));
    }
}

#[async_trait]
impl Actor for TcpClientActor {
    type Msg = TcpClientMessage;
    type State = TcpClientState;
    type Arguments = TcpClientArgs;

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        args: TcpClientArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        // Keep the "init" span short-lived and add the key identifiers as fields.
        let init_span = info_span!(
            "cassini.tcp_client.init",
            broker_addr = %args.config.broker_endpoint,
            server_name = %args.config.server_name,
            has_registration_id = args.registration_id.is_some(),
        );
        let _g = init_span.enter();
        info!("Starting TCP Client");

        // --- 1. Initialize crypto provider once ---
        let _g = debug_span!("tls.install_crypto_provider").entered();
        match rustls::crypto::aws_lc_rs::default_provider().install_default() {
            Ok(_) => warn!("Default crypto provider installed (this should only happen once)"),
            Err(_) => debug!("Crypto provider already configured"),
        }
        drop(_g);

        // --- 2. Load CA certificate into root store ---
        let _g = debug_span!("tls.load_ca_cert").entered();
        let mut root_cert_store = rustls::RootCertStore::empty();
        let ca_path = &args.config.ca_certificate_path;

        let ca_cert = CertificateDer::from_pem_file(ca_path).map_err(|e| {
            ActorProcessingErr::from(anyhow::anyhow!(
                "Failed to read CA certificate from {ca_path}: {e}"
            ))
        })?;

        root_cert_store.add(ca_cert).map_err(|e| {
            ActorProcessingErr::from(anyhow::anyhow!(
                "Failed to add CA certificate to root store: {e}"
            ))
        })?;
        drop(_g);

        // --- 3. Build WebPKI verifier ---
        let _g = debug_span!("tls.build_verifier").entered();
        let verifier = WebPkiServerVerifier::builder(Arc::new(root_cert_store))
            .build()
            .map_err(|e| {
                ActorProcessingErr::from(anyhow::anyhow!("Failed to build WebPKI verifier: {e}"))
            })?;
        drop(_g);

        // --- 4. Load client cert and key ---
        let _g = debug_span!("tls.load_client_cert_key").entered();
        let mut certs = Vec::new();
        let cert_path = &args.config.client_certificate_path;
        let key_path = &args.config.client_key_path;

        let client_cert = CertificateDer::from_pem_file(cert_path).map_err(|e| {
            ActorProcessingErr::from(anyhow::anyhow!(
                "Failed to read client certificate from {cert_path}: {e}"
            ))
        })?;

        certs.push(client_cert);

        let private_key = PrivateKeyDer::from_pem_file(key_path).map_err(|e| {
            ActorProcessingErr::from(anyhow::anyhow!(
                "Failed to read client private key from {key_path}: {e}"
            ))
        })?;
        drop(_g);

        // --- 5. Build Rustls client config ---
        let _g = debug_span!("tls.build_client_config").entered();
        let client_config = rustls::ClientConfig::builder()
            .with_webpki_verifier(verifier)
            .with_client_auth_cert(certs, private_key)
            .map_err(|e| {
                ActorProcessingErr::from(anyhow::anyhow!(
                    "Failed to build Rustls client config: {e}"
                ))
            })?;
        drop(_g);

        // --- 6. Determine registration state ---
        let registration = match args.registration_id {
            Some(id) => RegistrationState::Registered {
                registration_id: id,
            },
            None => RegistrationState::Unregistered,
        };

        // --- 7. Construct final state ---
        // init_span ends when this function returns (no long-lived spawned tasks here)
        let events_output = args.events_output
            .unwrap_or_else(|| Arc::new(OutputPort::default()));

        let state = TcpClientState {
            bind_addr: args.config.broker_endpoint,
            server_name: args.config.server_name,
            reader: None,
            writer: None,
            registration,
            client_config: Arc::new(client_config),
            events_output,
            abort_handle: None,
            event_handler: args.event_handler,
        };

        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let addr = state.bind_addr.clone();
        let connect_span = info_span!(
            "cassini.tcp_client.connect",
            broker_addr = %addr,
            server_name = %state.server_name,
        );
        let _g = connect_span.enter();
        info!("Connecting to {addr}");

        let tls_stream = TcpClientActor::connect_with_backoff(
            addr.clone(),
            state.server_name.clone(),
            &state.client_config,
        )
        .await?;

        info!("TLS connection established");

        let (reader, write_half) = split(tls_stream);
        let writer = tokio::io::BufWriter::new(write_half);

        state.reader = Some(reader);
        state.writer = Some(Arc::new(Mutex::new(writer)));

        info!("Successfully connected to {addr}. Listening for messages.");

        // Spawn the long-running reader loop under its own span (spawn_reader should instrument the task).
        self.spawn_reader(myself.clone(), state)?;

        // Kick off registration *after* connection is guaranteed
        info!("Requesting registration");
        myself
            .send_message(TcpClientMessage::Register)
            .map_err(ActorProcessingErr::from)?;

        Ok(())
    }

    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Successfully stopped {myself:?}");
        if let Some(handle) = state.abort_handle.take() {
            handle.abort();
        }
        Ok(())
    }
    #[tracing::instrument(
        name = "cassini.tcp_client.handle",
        skip(self, myself, state),
        fields(registration = ?state.registration, msg = ?message),
    )]
    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match (&mut state.registration, message) {
            // ---- SetEventHandler (always handled) ----
            (_, TcpClientMessage::SetEventHandler(handler)) => {
                state.event_handler = Some(handler);
            }

            // ---- Reconnected (always handled) ----
            (_, TcpClientMessage::Reconnected { reader, writer }) => {
                state.reader = reader;
                state.writer = Some(writer);
                self.spawn_reader(myself.clone(), state)?;
            }

            // ---- Reconnect (only when registered) ----
            (RegistrationState::Registered { .. }, TcpClientMessage::Reconnect) => {
                info!("Attempting to reconnect...");
                state.writer = None;
                if let Some(handle) = state.abort_handle.take() {
                    handle.abort();
                }

                let addr = state.bind_addr.clone();
                let server_name = state.server_name.clone();
                let client_config = state.client_config.clone();
                let myself_clone = myself.clone();
                tokio::spawn(async move {
                    match TcpClientActor::connect_with_backoff(addr, server_name, &client_config).await {
                        Ok(tls_stream) => {
                            let (reader, write_half) = split(tls_stream);
                            let writer = tokio::io::BufWriter::new(write_half);
                            let _ = myself_clone.send_message(TcpClientMessage::Reconnected {
                                reader: Some(reader),
                                writer: Arc::new(Mutex::new(writer)),
                            });
                            let _ = myself_clone.send_message(TcpClientMessage::Register);
                        }
                        Err(e) => {
                            error!("Reconnection failed: {}", e);
                            myself_clone.stop(Some("Reconnection failed".to_string()));
                        }
                    }
                });
            }

            // ---- Unregistered state ----
            (RegistrationState::Unregistered, TcpClientMessage::Register) => {
                let envelope = ClientMessage::RegistrationRequest {
                    registration_id: None,
                    trace_ctx: Self::current_trace_ctx(),
                };
                if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                    TcpClientActor::emit_transport_err_and_stop(myself, state, e.to_string())
                }
            }

            (RegistrationState::Unregistered, TcpClientMessage::RegistrationResponse(registration_id)) => {
                state.registration = RegistrationState::Registered {
                    registration_id: registration_id.clone(),
                };
                let event = ClientEvent::Registered { registration_id };
                state.events_output.send(event.clone());
                if let Some(handler) = &state.event_handler {
                    if let Err(e) = handler.send_message(event) {
                        error!("Failed to send Registered to direct handler: {}", e);
                    }
                }
            }

            (RegistrationState::Unregistered, _) => {
                warn!("Ignoring message, client is unregistered.");
            }

            // ---- Registered state ----
            (RegistrationState::Registered { registration_id }, message) => {
                match message {
                    TcpClientMessage::Register => {
                        let envelope = ClientMessage::RegistrationRequest {
                            registration_id: Some(registration_id.to_owned()),
                            trace_ctx: Self::current_trace_ctx(),
                        };
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            TcpClientActor::emit_transport_err_and_stop(myself, state, e.to_string())
                        }
                    }
                    TcpClientMessage::GetRegistrationId(reply) => {
                        reply.send(Some(registration_id.to_owned())).ok();
                    }
                    TcpClientMessage::Publish { topic, payload, trace_ctx } => {
                        let envelope = ClientMessage::PublishRequest {
                            topic,
                            payload: payload.into(),
                            registration_id: registration_id.to_owned(),
                            trace_ctx,
                        };
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            TcpClientActor::emit_transport_err_and_stop(myself, state, e.to_string());
                        }
                    }
                    TcpClientMessage::Subscribe { topic, trace_ctx } => {
                        let envelope = ClientMessage::SubscribeRequest {
                            registration_id: registration_id.to_owned(),
                            topic,
                            trace_ctx,
                        };
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            TcpClientActor::emit_transport_err_and_stop(myself, state, e.to_string());
                        }
                    }
                    TcpClientMessage::Disconnect { trace_ctx } => {
                        info!("Received disconnect signal. Shutting down client.");
                        debug!("Flushing pending writes before disconnect...");
                        sleep(Duration::from_millis(100)).await;
                        let envelope = ClientMessage::DisconnectRequest {
                            registration_id: Some(registration_id.to_owned()),
                            trace_ctx,
                        };
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            TcpClientActor::emit_transport_err_and_stop(myself.clone(), state, e.to_string());
                        }
                        if let Some(handle) = state.abort_handle.take() {
                            handle.abort();
                        }
                        myself.stop(Some(CLIENT_DISCONNECTED.to_string()));
                    }
                    TcpClientMessage::UnsubscribeRequest { topic, trace_ctx } => {
                        let envelope = ClientMessage::UnsubscribeRequest {
                            registration_id: registration_id.to_owned(),
                            topic,
                            trace_ctx,
                        };
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            TcpClientActor::emit_transport_err_and_stop(myself.clone(), state, e.to_string());
                        }
                    }
                    TcpClientMessage::ErrorMessage(error) => {
                        warn!("Received error from broker: {error}");
                    }
                    TcpClientMessage::ListSessions { trace_ctx } => {
                        let envelope = ClientMessage::ControlRequest {
                            registration_id: registration_id.to_owned(),
                            op: ControlOp::ListSessions,
                            trace_ctx,
                        };
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            TcpClientActor::emit_transport_err_and_stop(myself.clone(), state, e.to_string());
                        }
                    }
                    TcpClientMessage::ListTopics { trace_ctx } => {
                        let envelope = ClientMessage::ControlRequest {
                            registration_id: registration_id.to_owned(),
                            op: ControlOp::ListTopics,
                            trace_ctx,
                        };
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            TcpClientActor::emit_transport_err_and_stop(myself.clone(), state, e.to_string());
                        }
                    }
                    TcpClientMessage::ControlRequest { op, trace_ctx } => {
                        let envelope = ClientMessage::ControlRequest {
                            registration_id: registration_id.to_owned(),
                            op,
                            trace_ctx,
                        };
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            TcpClientActor::emit_transport_err_and_stop(myself.clone(), state, e.to_string());
                        }
                    }
                    // Ignore other messages like Reconnect (already handled at top level)
                    _ => (),
                }
            }
        }
        Ok(())
    }
}
