use cassini_types::{ClientEvent, ClientMessage, ControlOp};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort};
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

pub const UNEXPECTED_DISCONNECT: &str = "UNEXPECTED_DISCONNECT";
pub const REGISTRATION_EXPECTED: &str =
    "Expected client to be registered to conduct this operation.";

pub const CLIENT_DISCONNECTED: &str = "CLIENT_DISCONNECTED";

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
    },
    Subscribe(String),
    /// Unsubscribe request from the client.
    /// Contains the topic
    UnsubscribeRequest(String),
    ///Disconnect, sending a session id to end, if any
    Disconnect,
    ListSessions,
    ListTopics,
    GetSession,
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
}

pub struct TcpClientArgs {
    pub config: TCPClientConfig,
    pub registration_id: Option<String>,
    pub events_output: Arc<OutputPort<ClientEvent>>,
}

/// TCP client actor
pub struct TcpClientActor;

impl TcpClientActor {
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

        // OWNED ServerName with 'static lifetime
        let server_name = ServerName::try_from(server_name)
            .map_err(|_| ActorProcessingErr::from("Invalid DNS name"))?;

        for attempt in 1..=max_attempts {
            let backoff_ms = backoff.as_millis();
            warn!(attempt, backoff_ms, "connect failed; retrying");
            match TcpStream::connect(addr.clone()).await {
                Ok(tcp) => match connector.connect(server_name.clone(), tcp).await {
                    Ok(tls) => return Ok(tls),
                    Err(e) => {
                        debug!(attempt, error = %e, "TLS handshake failed, retrying");
                    }
                },
                Err(e) => {
                    debug!(attempt, error = %e, "TCP connect failed, retrying");
                }
            }

            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(max_backoff);
        }

        Err(ActorProcessingErr::from(
            "Failed to connect to broker after retries",
        ))
    }

    fn spawn_reader(
        &self,
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
    ) -> Result<(), ActorProcessingErr> {
        // Take ownership of the read half exactly once.
        let reader = state.reader.take().expect("Reader already taken");

        let queue_out = state.events_output.clone();
        let broker_addr = state.bind_addr.clone();
        // If you have a stable ID, use it here; otherwise addr is still useful.
        // (You can add registration_id later once known.)
        let read_loop_span = info_span!(
            "cassini.tcp_client.read_loop",
            broker_addr = %broker_addr,
        );

        let abort = tokio::spawn(async move {

            let mut buf_reader = tokio::io::BufReader::new(reader);
            loop {
                match buf_reader.read_u32().await {
                    Ok(incoming_msg_length) => {
                        if incoming_msg_length > 0 {
                            let mut buffer = vec![0; incoming_msg_length as usize];

                            trace!("Reading {incoming_msg_length} byte(s).");
                            if let Ok(_) = buf_reader.read_exact(&mut buffer).await {
                                // Now we *actually* have a frame. Scope the span to just decode+dispatch.
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
                                            ClientMessage::RegistrationResponse { result } => {
                                                match result {
                                                    Ok(registration_id) => {
                                                        //emit registration event for consumers
                                                        myself
                                                            .send_message(
                                                                TcpClientMessage::RegistrationResponse(
                                                                    registration_id,
                                                                ),
                                                            )
                                                            .expect("Could not forward message to {myself:?");
                                                    }
                                                    Err(e) => {
                                                        warn!(
                                                            "Failed to register session with the server. {e}"
                                                        );
                                                        //TODO: We practically never fail to register unless there's some sort of connection error.
                                                        // Should this change, how do we want to react?
                                                    }
                                                }
                                            }
                                            ClientMessage::PublishResponse {
                                                topic,
                                                payload,
                                                result,
                                            } => {
                                                //new message on topic
                                                if result.is_ok() {
                                                    queue_out
                                                        .send(ClientEvent::MessagePublished { topic, payload });
                                                } else {
                                                    warn!("Failed to publish message to topic: {topic}");
                                                }
                                            }
                                            ClientMessage::SubscribeAcknowledgment { topic, result } => {
                                                if result.is_ok() {
                                                    info!("Successfully subscribed to topic: {topic}");
                                                } else {
                                                    warn!("Failed to subscribe to topic: {topic}, {result:?}");
                                                }
                                            }

                                            ClientMessage::UnsubscribeAcknowledgment { topic, result } => {
                                                if result.is_ok() {
                                                    info!("Successfully unsubscribed from topic: {topic}");
                                                } else {
                                                    warn!("Failed to unsubscribe from topic: {topic}");
                                                }
                                            }
                                            _ => (),
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
                        // This is the main "lifetime end" signal for the read loop.
                        // Use warn so it stands out in traces; it usually means disconnect.
                        warn!(error = %e, "TCP reader terminating");
                        queue_out.send(ClientEvent::TransportError {
                            reason: format!("{UNEXPECTED_DISCONNECT} {e}")
                        });
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
        match rkyv::to_bytes::<Error>(&message) {
            Ok(bytes) => {
                //create new buffer
                let mut buffer = Vec::new();

                //get message length as header
                let len = (bytes.len() as u64).to_be_bytes();
                buffer.extend_from_slice(&len);

                //add message to buffer
                buffer.extend_from_slice(&bytes);

                //write message
                if let Some(arc_writer) = state.writer.as_ref() {
                    let mut writer = arc_writer.lock().await;
                    writer.write_all(&buffer).await?;
                    writer.flush().await?;
                    Ok(())
                } else {
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
        let state = TcpClientState {
            bind_addr: args.config.broker_endpoint,
            server_name: args.config.server_name,
            reader: None,
            writer: None,
            registration,
            client_config: Arc::new(client_config),
            events_output: args.events_output,
            abort_handle: None,
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
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Successfully stopped {myself:?}");
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
            // If we're unregistered, register
            (RegistrationState::Unregistered, TcpClientMessage::Register) => {
                let envelope = ClientMessage::RegistrationRequest {
                    registration_id: None,
                };
                if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                    TcpClientActor::emit_transport_err_and_stop(myself, state, e.to_string())
                }
            }
            // We might've initialized with a registration id,
            // but we need to actually tell the broker to restore our session.
            (RegistrationState::Registered { registration_id }, TcpClientMessage::Register) => {
                let envelope = ClientMessage::RegistrationRequest {
                    registration_id: Some(registration_id.to_owned()),
                };
                if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                    TcpClientActor::emit_transport_err_and_stop(myself, state, e.to_string())
                }
            }
            (
                RegistrationState::Registered { registration_id },
                TcpClientMessage::GetRegistrationId(reply),
            ) => {
                // in this case, no one should be asking for the session id if we're not registered, but if they do, we'll ignore them.
                // We also don't really care if they fail to receive it, this actor is useless on its own.
                reply.send(Some(registration_id.to_owned())).ok();
            }
            (
                RegistrationState::Unregistered,
                TcpClientMessage::RegistrationResponse(registration_id),
            ) => {
                // update state and send
                state.registration = RegistrationState::Registered {
                    registration_id: registration_id.clone(),
                };
                // emit event
                state
                    .events_output
                    .send(ClientEvent::Registered { registration_id });
            }

            // ignore all other messages while unregistered.
            (RegistrationState::Unregistered, _) => {
                warn!("Ignoring message, client is unregistered with a cassini instance.");
            }
            (RegistrationState::Registered { registration_id }, message) => {
                // handle message variants
                match message {
                    TcpClientMessage::Publish { topic, payload } => {
                        let envelope = ClientMessage::PublishRequest {
                            topic,
                            payload,
                            registration_id: registration_id.to_owned(),
                        };
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            TcpClientActor::emit_transport_err_and_stop(
                                myself,
                                state,
                                e.to_string(),
                            );
                        }
                    }
                    TcpClientMessage::Subscribe(topic) => {
                        let envelope = ClientMessage::SubscribeRequest {
                            registration_id: registration_id.to_owned(),
                            topic,
                        };
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            TcpClientActor::emit_transport_err_and_stop(
                                myself,
                                state,
                                e.to_string(),
                            );
                        }
                    }
                    TcpClientMessage::Disconnect => {
                        info!("Received disconnect signal. Shutting down client.",);
                        let envelope =
                            ClientMessage::DisconnectRequest(Some(registration_id.to_owned()));
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            TcpClientActor::emit_transport_err_and_stop(
                                myself.clone(),
                                state,
                                e.to_string(),
                            );
                        }
                        // if we're disconnecting explicitly, we're  good to just stop here.
                        state.abort_handle.as_ref().map(|handle| handle.abort());

                        myself.stop(Some(CLIENT_DISCONNECTED.to_string()));
                    }
                    TcpClientMessage::UnsubscribeRequest(topic) => {
                        let envelope = ClientMessage::UnsubscribeRequest {
                            registration_id: registration_id.to_owned(),
                            topic,
                        };
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            TcpClientActor::emit_transport_err_and_stop(
                                myself.clone(),
                                state,
                                e.to_string(),
                            );
                        }
                    }
                    TcpClientMessage::ErrorMessage(error) => {
                        warn!("Received error from broker: {error}");
                    }
                    TcpClientMessage::ListSessions => {
                        let envelope = ClientMessage::ControlRequest {
                            registration_id: registration_id.to_owned(),
                            op: ControlOp::ListSessions,
                        };

                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            TcpClientActor::emit_transport_err_and_stop(
                                myself.clone(),
                                state,
                                e.to_string(),
                            );
                        }
                    }
                    TcpClientMessage::ListTopics => {
                        let envelope = ClientMessage::ControlRequest {
                            registration_id: registration_id.to_owned(),
                            op: ControlOp::ListTopics,
                        };

                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            TcpClientActor::emit_transport_err_and_stop(
                                myself.clone(),
                                state,
                                e.to_string(),
                            );
                        }
                    }
                    // other variants handled elsewhere
                    _ => (),
                }
            }
        }
        Ok(())
    }
}
