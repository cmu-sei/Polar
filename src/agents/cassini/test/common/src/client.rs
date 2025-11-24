use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, async_trait};
use rkyv::deserialize;
use rkyv::rancor::Error;
use rustls::ClientConfig;
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use std::env;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncReadExt, BufWriter, ReadHalf, WriteHalf, split};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::task::AbortHandle;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tracing::{debug, error, info, warn};

use crate::{ArchivedHarnessControllerMessage, HarnessControllerMessage};

pub const UNEXPECTED_DISCONNECT: &str = "UNEXPECTED_DISCONNECT";

///
/// A base configuration for a TCP Client actor
pub struct HarnessClientConfig {
    pub sink_endpoint: String,
    pub server_name: String,
    pub ca_certificate_path: String,
    pub client_certificate_path: String,
    pub client_key_path: String,
}

impl HarnessClientConfig {
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

        HarnessClientConfig {
            sink_endpoint,
            server_name,
            ca_certificate_path,
            client_certificate_path,
            client_key_path,
        }
    }
}

pub struct HarnessClientArgs {
    pub config: HarnessClientConfig,
}

/// Actor state for the TCP client
pub struct HarnessClientState {
    bind_addr: String,
    server_name: String,
    writer: Option<Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>, // Use Option to allow taking ownership
    // output_port: Arc<OutputPort<HarnessControllerMessage>>,
    abort_handle: Option<AbortHandle>, // abort handle switch for the stream read loop
    client_config: Arc<ClientConfig>,  // tls client configuration
}

/// Messages handled by the TCP client actor
#[derive(Debug)]
pub enum HarnessClientMessage {
    Send(HarnessControllerMessage),
    // Disconnect,
}

/// TCP client actor
pub struct HarnessClient;

impl HarnessClient {
    /// Send a payload to the sink over the wire.
    pub async fn send_message(
        message: HarnessControllerMessage,
        state: &mut HarnessClientState,
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
impl Actor for HarnessClient {
    type Msg = HarnessClientMessage;
    type State = HarnessClientState;
    type Arguments = HarnessClientArgs;

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        args: HarnessClientArgs,
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

        let state = HarnessClientState {
            bind_addr: args.config.sink_endpoint,
            server_name: args.config.server_name,
            reader: None,
            writer: None,
            client_config: Arc::new(config),
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

                        //start listening
                        let abort_handle = tokio::spawn(async move {
                            let mut buf_reader = tokio::io::BufReader::new(reader);

                            while let Ok(incoming_msg_length) = buf_reader.read_u32().await {
                                if incoming_msg_length > 0 {
                                    let mut buffer = vec![0; incoming_msg_length as usize];
                                    if let Ok(_) = buf_reader.read_exact(&mut buffer).await {

                                        match rkyv::access::<ArchivedHarnessControllerMessage, Error>(&buffer[..]) {
                                            Ok(archived) => {
                                                // deserialize back to the original type
                                                if let Ok(deserialized) =
                                                    deserialize::<HarnessControllerMessage, Error>(archived)
                                                {
                                                    myself.try_get_supervisor().map(|service| {
                                                        service
                                                            .send_message(deserialized)
                                                            .expect("Expected to forward command upwards");
                                                    });
                                                }
                                            }
                                            Err(e) => {
                                                warn!("Failed to parse message: {e}");
                                            }
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
                // If we failed to connect to the sink, we should just stop.
                myself.notify_supervisor(ractor::SupervisionEvent::ActorFailed(
                    myself.get_cell(),
                    ActorProcessingErr::from(e),
                ));
                myself.stop(None);
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
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            HarnessClientMessage::Send(command) => {
                HarnessClient::send_message(command, state)
                    .await
                    .expect("Expected to send message successfully");
            }
        }
        Ok(())
    }
}
