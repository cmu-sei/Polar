use cassini_types::{ArchivedClientMessage, ClientMessage};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort};
use rkyv::deserialize;
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
use tracing::{debug, error, info, info_span, warn};

pub const UNEXPECTED_DISCONNECT: &str = "UNEXPECTED_DISCONNECT";
pub const REGISTRATION_EXPECTED: &str =
    "Expected client to be registered to conduct this operation.";

///
/// A basse configuration for a TCP Client actor
pub struct TCPClientConfig {
    pub broker_endpoint: String,
    pub server_name: String,
    pub ca_certificate_path: String,
    pub client_certificate_path: String,
    pub client_key_path: String,
}

pub enum RegistrationState {
    Unregistered,
    /// Eventually, we might want to a stronger type definition for the registration token
    /// perhaps one day we'll treat it as a true secret and add some protections?
    Registered {
        registration_id: String,
    },
}

impl TCPClientConfig {
    /// Read filepaths from the environment and return. If we can't read these, we can't start
    pub fn new() -> Self {
        let client_certificate_path =
            env::var("TLS_CLIENT_CERT").expect("Expected a value for TLS_CLIENT_CERT.");
        let client_key_path =
            env::var("TLS_CLIENT_KEY").expect("Expected a value for TLS_CLIENT_KEY.");
        let ca_certificate_path =
            env::var("TLS_CA_CERT").expect("Expected a value for TLS_CA_CERT.");
        let broker_endpoint =
            env::var("BROKER_ADDR").expect("Expected a valid socket address for BROKER_ADDR");
        let server_name =
            env::var("CASSINI_SERVER_NAME").expect("Expected a value for CASSINI_SERVER_NAME");

        TCPClientConfig {
            broker_endpoint,
            server_name,
            ca_certificate_path,
            client_certificate_path,
            client_key_path,
        }
    }
}

/// Messages handled by the TCP client actor
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
}

/// Actor state for the TCP client
pub struct TcpClientState {
    bind_addr: String,
    server_name: String,
    writer: Option<Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>, // Use Option to allow taking ownership
    registration: RegistrationState,
    client_config: Arc<ClientConfig>,
    output_port: Arc<OutputPort<String>>,
    queue_output: Arc<OutputPort<Vec<u8>>>,
    abort_handle: Option<AbortHandle>, // abort handle switch for the stream read loop
}

pub struct TcpClientArgs {
    pub config: TCPClientConfig,
    pub registration_id: Option<String>,
    pub output_port: Arc<OutputPort<String>>,
    pub queue_output: Arc<OutputPort<Vec<u8>>>,
}

/// TCP client actor
pub struct TcpClientActor;

impl TcpClientActor {
    /// Send a payload to the broker over the write.
    pub async fn send_message(
        message: ClientMessage,
        state: &mut TcpClientState,
    ) -> Result<(), Box<dyn std::error::Error>> {
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
                let unwrapped_writer = state.writer.clone().unwrap();
                let mut writer = unwrapped_writer.lock().await;
                if let Err(e) = writer.write_all(&buffer).await {
                    error!("Failed to flush stream {e}");
                    return Err(Box::new(e));
                }

                if let Err(e) = writer.flush().await {
                    error!("Failed to flush stream {e}");
                    return Err(Box::new(e));
                }

                Ok(())
            }
            Err(e) => {
                warn!("Failed to serialize message. {e}");
                Err(Box::new(e))
            }
        }
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
        let _span = info_span!("cassini.tcp-client.init").entered();
        info!("Starting TCP Client");

        // --- 1. Initialize crypto provider once ---
        match rustls::crypto::aws_lc_rs::default_provider().install_default() {
            Ok(_) => warn!("Default crypto provider installed (this should only happen once)"),
            Err(_) => debug!("Crypto provider already configured"),
        }

        // --- 2. Load CA certificate into root store ---
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

        // --- 3. Build WebPKI verifier ---
        let verifier = WebPkiServerVerifier::builder(Arc::new(root_cert_store))
            .build()
            .map_err(|e| {
                ActorProcessingErr::from(anyhow::anyhow!("Failed to build WebPKI verifier: {e}"))
            })?;

        // --- 4. Load client cert and key ---
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

        // --- 5. Build Rustls client config ---
        let client_config = rustls::ClientConfig::builder()
            .with_webpki_verifier(verifier)
            .with_client_auth_cert(certs, private_key)
            .map_err(|e| {
                ActorProcessingErr::from(anyhow::anyhow!(
                    "Failed to build Rustls client config: {e}"
                ))
            })?;

        // --- 6. Determine registration state ---
        let registration = match args.registration_id {
            Some(id) => RegistrationState::Registered {
                registration_id: id,
            },
            None => RegistrationState::Unregistered,
        };

        // --- 7. Construct final state ---
        let state = TcpClientState {
            bind_addr: args.config.broker_endpoint,
            server_name: args.config.server_name,
            reader: None,
            writer: None,
            registration,
            client_config: Arc::new(client_config),
            output_port: args.output_port,
            queue_output: args.queue_output,
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
        info!("Connecting to {addr}");
        let connector = TlsConnector::from(Arc::clone(&state.client_config));

        match TcpStream::connect(&addr).await {
            Ok(tcp_stream) => {
                let domain =
                    ServerName::try_from(state.server_name.clone()).expect("invalid DNS name");

                match connector.connect(domain, tcp_stream).await {
                    Ok(tls_stream) => {
                        let (reader, write_half) = split(tls_stream);

                        let writer = tokio::io::BufWriter::new(write_half);

                        state.reader = Some(reader);
                        state.writer = Some(Arc::new(Mutex::new(writer)));

                        info!("{myself:?} Listening... ");
                        let reader = tokio::io::BufReader::new(
                            state.reader.take().expect("Reader already taken!"),
                        );

                        let cloned_self = myself.clone();
                        let cloned_queue_out = state.queue_output.clone();

                        //start listening
                        let abort_handle = tokio::spawn(async move {
                            let mut buf_reader = tokio::io::BufReader::new(reader);

                            while let Ok(incoming_msg_length) = buf_reader.read_u32().await {
                                if incoming_msg_length > 0 {
                                    let mut buffer = vec![0; incoming_msg_length as usize];
                                    if let Ok(_) = buf_reader.read_exact(&mut buffer).await {
                                        let archived =
                                            rkyv::access::<ArchivedClientMessage, Error>(
                                                &buffer[..],
                                            )
                                            .unwrap();
                                        // And you can always deserialize back to the original type

                                        if let Ok(message) =
                                            deserialize::<ClientMessage, Error>(archived)
                                        {
                                            match message {
                                                ClientMessage::RegistrationResponse {
                                                    result
                                                } => {
                                                    match result {
                                                        Ok(registration_id) => {
                                                            //emit registration event for consumers
                                                            cloned_self.send_message(TcpClientMessage::RegistrationResponse(registration_id)).expect("Could not forward message to {myself:?");
                                                        }
                                                        Err(e) => {
                                                            info!("Failed to register session with the server. {e}");
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
                                                        cloned_queue_out.send(payload);
                                                    } else {
                                                        warn!("Failed to publish message to topic: {topic}");
                                                    }
                                                }
                                                ClientMessage::SubscribeAcknowledgment {
                                                    topic,
                                                    result,
                                                } => {
                                                    if result.is_ok() {
                                                        info!("Successfully subscribed to topic: {topic}");
                                                    } else {
                                                        warn!("Failed to subscribe to topic: {topic}, {result:?}");
                                                    }
                                                }

                                                ClientMessage::UnsubscribeAcknowledgment {
                                                    topic,
                                                    result,
                                                } => {
                                                    if result.is_ok() {
                                                        info!("Successfully unsubscribed from topic: {topic}");
                                                    } else {
                                                        warn!("Failed to unsubscribe from topic: {topic}");
                                                    }
                                                }
                                                _ => (),
                                            }
                                        }
                                    }
                                }
                            }
                        }).abort_handle();

                        state.abort_handle = Some(abort_handle);
                    }
                    Err(e) => {
                        error!("Failed to establish mTLS connection! {e}");
                        myself.stop(Some("Failed to establish mTLS connection! {e}".to_string()));
                    }
                }
            }
            Err(e) => {
                let err = format!("Failed to connect to server: {e}");
                error!("{err}");
                // TODO: Maybe this should be an enum/constant we can match instead?
                return Err(ActorProcessingErr::from(err));
            }
        };

        //Send registration request
        if let Err(e) = myself.send_message(TcpClientMessage::Register) {
            error!("{e}");
            return Err(ActorProcessingErr::from(e));
        }

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
                    myself.stop(Some(format!("Unexpected error sending message. {e}")))
                }
            }
            // We might've initialized with a registration id,
            // but we need to actually tell the broker to restore our session.
            (RegistrationState::Registered { registration_id }, TcpClientMessage::Register) => {
                let envelope = ClientMessage::RegistrationRequest {
                    registration_id: Some(registration_id.to_owned()),
                };
                if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                    myself.stop(Some(format!("Unexpected error sending message. {e}")))
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
                state.output_port.send(registration_id);
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
                            myself.stop(Some(format!("Unexpected error sending message. {e}")))
                        }
                    }
                    TcpClientMessage::Subscribe(topic) => {
                        let envelope = ClientMessage::SubscribeRequest {
                            registration_id: registration_id.to_owned(),
                            topic,
                        };
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            myself.stop(Some(format!("Unexpected error sending message. {e}")))
                        }
                    }
                    TcpClientMessage::Disconnect => {
                        info!("Received disconnect signal. Shutting down client.",);
                        let envelope =
                            ClientMessage::DisconnectRequest(Some(registration_id.to_owned()));
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            myself.stop(Some(format!("Unexpected error sending message. {e}")))
                        }
                        // if we're disconnecting explicitly, we're  good to just stop here.
                        state.abort_handle.as_ref().map(|handle| handle.abort());

                        myself.stop(None);
                    }
                    TcpClientMessage::UnsubscribeRequest(topic) => {
                        let envelope = ClientMessage::UnsubscribeRequest {
                            registration_id: registration_id.to_owned(),
                            topic,
                        };
                        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                            myself.stop(Some(format!("Unexpected error sending message. {e}")))
                        }
                    }
                    TcpClientMessage::ErrorMessage(error) => {
                        warn!("Received error from broker: {error}");
                    }
                    // other variats handled elsewhere
                    _ => (),
                }
            }
        }
        Ok(())
    }
}
