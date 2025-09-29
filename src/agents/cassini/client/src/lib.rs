use cassini_types::{ArchivedClientMessage, ClientMessage};
use polar::UNEXPECTED_MESSAGE_STR;
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
use tracing::{debug, error, info, warn};

pub const UNEXPECTED_DISCONNECT: &str = "UNEXPECTED_DISCONNECT";

///
/// A basse configuration for a TCP Client actor
pub struct TCPClientConfig {
    pub broker_endpoint: String,
    pub server_name: String,
    pub ca_certificate_path: String,
    pub client_certificate_path: String,
    pub client_key_path: String,
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
    Register(Option<String>),
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
    registration_id: Option<String>,
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

                return Ok(());
            }
            Err(e) => {
                warn!("Failed to serialize message. {e}");
                return Err(Box::new(e));
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
        info!("Starting TCP Client ");
        // install default crypto provider
        let provider = rustls::crypto::aws_lc_rs::default_provider().install_default();
        if let Err(_) = provider {
            debug!("Crypto provider configured");
        }

        let mut root_cert_store = rustls::RootCertStore::empty();
        let _ = root_cert_store.add(
            CertificateDer::from_pem_file(args.config.ca_certificate_path.clone()).expect(
                &format!(
                    "Expected to read CA cert as a PEM file from {}",
                    args.config.ca_certificate_path
                ),
            ),
        );

        let client_cert = CertificateDer::from_pem_file(
            args.config.client_certificate_path.clone(),
        )
        .expect(&format!(
            "Expected to read client cert as a PEM file from {}",
            args.config.client_certificate_path
        ));
        let private_key =
            PrivateKeyDer::from_pem_file(args.config.client_key_path.clone()).expect(&format!(
                "Expected to read client key as a PEM file from {}",
                args.config.client_certificate_path
            ));
        let verifier = WebPkiServerVerifier::builder(Arc::new(root_cert_store))
            .build()
            .expect("Expected to build client verifier");
        let mut certs = Vec::new();
        certs.push(client_cert);

        let config = rustls::ClientConfig::builder()
            .with_webpki_verifier(verifier)
            .with_client_auth_cert(certs, private_key)
            .unwrap();

        let state = TcpClientState {
            bind_addr: args.config.broker_endpoint,
            server_name: args.config.server_name,
            reader: None,
            writer: None,
            registration_id: args.registration_id,
            client_config: Arc::new(config),
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
                                                    registration_id,
                                                    success,
                                                    error,
                                                } => {
                                                    if success {
                                                        info!("Successfully began session with id: {registration_id}");
                                                        //emit registration event for consumers
                                                        cloned_self.send_message(TcpClientMessage::RegistrationResponse(registration_id)).expect("Could not forward message to {myself:?");
                                                    } else {
                                                        warn!("Failed to register session with the server. {error:?}");
                                                        //TODO: We practically never fail to register unless there's some sort of connection error.
                                                        // Should this change, how do we want to react?
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
                error!("Failed to connect to server: {e}");
                myself.stop(Some("Failed to connect to server: {e}".to_string()));
            }
        };

        //Send registration request
        if let Err(e) =
            myself.send_message(TcpClientMessage::Register(state.registration_id.clone()))
        {
            error!("{e}");
            myself.stop(Some(format!("{e}")));
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
        match message {
            TcpClientMessage::Register(registration_id) => {
                let envelope = ClientMessage::RegistrationRequest { registration_id };
                if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                    myself.stop(Some(format!("Unexpected error sending message. {e}")))
                }
            }
            TcpClientMessage::Publish { topic, payload } => {
                let envelope = ClientMessage::PublishRequest {
                    topic,
                    payload,
                    registration_id: state.registration_id.clone(),
                };
                if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                    myself.stop(Some(format!("Unexpected error sending message. {e}")))
                }
            }
            TcpClientMessage::Subscribe(topic) => {
                let envelope = ClientMessage::SubscribeRequest {
                    registration_id: state.registration_id.clone(),
                    topic,
                };
                if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                    myself.stop(Some(format!("Unexpected error sending message. {e}")))
                }
            }
            TcpClientMessage::Disconnect => {
                info!(
                    "Received disconnect signal. Ending session {:?}",
                    state.registration_id
                );
                let envelope = ClientMessage::DisconnectRequest(state.registration_id.clone());
                if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                    myself.stop(Some(format!("Unexpected error sending message. {e}")))
                }
                // if we're disconnecting explicitly, we're  good to just stop here.
                state.abort_handle.as_ref().map(|handle| handle.abort());

                myself.stop(None);
            }
            TcpClientMessage::UnsubscribeRequest(topic) => {
                let envelope = ClientMessage::UnsubscribeRequest {
                    registration_id: state.registration_id.clone(),
                    topic,
                };
                if let Err(e) = TcpClientActor::send_message(envelope, state).await {
                    myself.stop(Some(format!("Unexpected error sending message. {e}")))
                }
            }
            TcpClientMessage::RegistrationResponse(registration_id) => {
                state.registration_id = Some(registration_id.clone());
                // emit event
                state.output_port.send(registration_id);
            }
            TcpClientMessage::GetRegistrationId(reply) => {
                if let Some(registration_id) = &state.registration_id {
                    reply
                        .send(Some(registration_id.to_owned()))
                        .expect("Expected to send registration_id to reply port");
                } else {
                    reply.send(None).expect("Expected to send default string");
                }
            }
            _ => warn!("{UNEXPECTED_MESSAGE_STR}"),
        }
        Ok(())
    }
}
