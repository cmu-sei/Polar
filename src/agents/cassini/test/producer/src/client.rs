use harness_common::*;
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
/// A base configuration for a TCP Client actor
pub struct SinkClientConfig {
    pub sink_endpoint: String,
    pub server_name: String,
    pub ca_certificate_path: String,
    pub client_certificate_path: String,
    pub client_key_path: String,
}

impl SinkClientConfig {
    /// Read filepaths from the environment and return. If we can't read these, we can't start
    pub fn new() -> Self {
        let client_certificate_path =
            env::var("TLS_CLIENT_CERT").expect("Expected a value for TLS_CLIENT_CERT.");
        let client_key_path =
            env::var("TLS_CLIENT_KEY").expect("Expected a value for TLS_CLIENT_KEY.");
        let ca_certificate_path =
            env::var("TLS_CA_CERT").expect("Expected a value for TLS_CA_CERT.");
        let sink_endpoint =
            env::var("SINK_ADDR").expect("Expected a valid socket address for SINK_ADDR");
        let server_name =
            env::var("SINK_SERVER_NAME").expect("Expected a value for SINK_SERVER_NAME");

        SinkClientConfig {
            sink_endpoint,
            server_name,
            ca_certificate_path,
            client_certificate_path,
            client_key_path,
        }
    }
}

pub struct SinkClientArgs {
    pub config: SinkClientConfig,
    pub output_port: Arc<OutputPort<ProducerMessage>>,
}

/// Actor state for the TCP client
pub struct SinkClientState {
    bind_addr: String,
    server_name: String,
    writer: Option<Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>, // Use Option to allow taking ownership
    output_port: Arc<OutputPort<ProducerMessage>>,
    abort_handle: Option<AbortHandle>, // abort handle switch for the stream read loop
    client_config: Arc<ClientConfig>,  // tls client configuration
}

/// Messages handled by the TCP client actor
#[derive(Debug)]
pub enum SinkClientMessage {
    Send(SinkCommand),
    // Disconnect,
}

/// TCP client actor
pub struct SinkClient;

impl SinkClient {
    /// Send a payload to the sink over the wire.
    pub async fn send_message(
        message: SinkCommand,
        state: &mut SinkClientState,
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
impl Actor for SinkClient {
    type Msg = SinkClientMessage;
    type State = SinkClientState;
    type Arguments = SinkClientArgs;

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        args: SinkClientArgs,
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

        let state = SinkClientState {
            bind_addr: args.config.sink_endpoint,
            server_name: args.config.server_name,
            reader: None,
            writer: None,
            client_config: Arc::new(config),
            output_port: args.output_port,
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

                        let cloned_output_port = state.output_port.clone();
                        //start listening
                        let abort_handle = tokio::spawn(async move {
                            let mut buf_reader = tokio::io::BufReader::new(reader);

                            while let Ok(incoming_msg_length) = buf_reader.read_u32().await {
                                if incoming_msg_length > 0 {
                                    let mut buffer = vec![0; incoming_msg_length as usize];
                                    if let Ok(_) = buf_reader.read_exact(&mut buffer).await {
                                        let archived =
                                            rkyv::access::<ArchivedProducerMessage, Error>(
                                                &buffer[..],
                                            )
                                            .unwrap();

                                        if let Ok(message) =
                                            deserialize::<ProducerMessage, Error>(archived)
                                        {
                                            cloned_output_port.send(message);
                                        }
                                    }
                                }
                            }
                        })
                        .abort_handle();

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
            SinkClientMessage::Send(command) => {
                SinkClient::send_message(command, state)
                    .await
                    .expect("Expected to send message successfully");
            }
        }
        Ok(())
    }
}
