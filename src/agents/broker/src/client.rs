use crate::{ArchivedClientMessage, ClientMessage, TCPClientConfig};
use polar::DispatcherMessage;
use ractor::registry::where_is;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort};
use rkyv::deserialize;
use rkyv::rancor::Error;
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::ClientConfig;
use std::sync::Arc;
use tokio::io::{split, AsyncReadExt, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tracing::{debug, error, info, warn};

/// Messages handled by the TCP client actor
pub enum TcpClientMessage {
    Send(ClientMessage),
    RegistrationResponse(String),
    ErrorMessage(String),
    /// Returns the registration id when queried, unlikely to be useful
    /// unless some other agent dies before the client does and needs to get it back
    GetRegistrationId(RpcReplyPort<Option<String>>),
    /// Message that gets emitted when the client successfully registers with the broker.
    /// It should contain a valid registration uid
    ClientRegistered(String),
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
}

pub struct TcpClientArgs {
    pub config: TCPClientConfig,
    pub registration_id: Option<String>,
    pub output_port: Arc<OutputPort<String>>,
}

/// TCP client actor
pub struct TcpClientActor;

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

                        //start listening
                        let _ = tokio::spawn(async move {
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
                                                        // try to find dispatcher
                                                        // NOTE: This represents a mandate to name all dispatcher actors with this string
                                                        // So far, we rely heavily on ractor's actor registry to do lookups to know where data is supposed to go.
                                                        let dispatcher =
                                                            where_is("DISPATCH".to_string())
                                                                .expect(
                                                                    "Expected to find dispatcher.",
                                                                );
                                                        dispatcher
                                                            .send_message(
                                                                DispatcherMessage::Dispatch {
                                                                    message: payload,
                                                                    topic,
                                                                },
                                                            )
                                                            .expect(
                                                                "Expected to send to dispatcher",
                                                            );
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
                        });
                    }
                    Err(e) => {
                        error!("Failed to establish mTLS connection! {e}");
                        myself.stop(Some("Failed to establish mTLS connection! {e}".to_string()));
                    }
                }
            }
            Err(e) => {
                // Given that the gitlab consumer recently got an upgrade using an exponential backoff,
                // perhaps it's time the client actor got one too?
                // It's not the end of the world if cassini goes down is it?
                // if it does, it drops messages, but that doesn't mean we have to give up on the client end.
                error!("Failed to connect to server: {e}");
                myself.stop(Some("Failed to connect to server: {e}".to_string()));
            }
        };

        //Send registration request
        if let Err(e) =
            myself.send_message(TcpClientMessage::Send(ClientMessage::RegistrationRequest {
                registration_id: state.registration_id.clone(),
            }))
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
            TcpClientMessage::Send(broker_msg) => {
                match rkyv::to_bytes::<Error>(&broker_msg) {
                    Ok(bytes) => {
                        //create new buffer
                        let mut buffer = Vec::new();

                        //get message length as header
                        let len = bytes.len().to_be_bytes();
                        buffer.extend_from_slice(&len);

                        //add message to buffer
                        buffer.extend_from_slice(&bytes);

                        //write message

                        let unwrapped_writer = state.writer.clone().unwrap();
                        let mut writer = unwrapped_writer.lock().await;
                        if let Err(e) = writer.write(&buffer).await {
                            error!("Failed to flush stream {e}, stopping client");
                            myself.stop(Some("UNEXPECTED_DISCONNECT".to_string()))
                        }

                        if let Err(e) = writer.flush().await {
                            error!("Failed to flush stream {e}, stopping client");
                            myself.stop(Some("UNEXPECTED_DISCONNECT".to_string()))
                        }
                    }
                    Err(e) => {
                        warn!("Failed to serialize message. {e}")
                    }
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
            _ => todo!(),
        }
        Ok(())
    }
}
