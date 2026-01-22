use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, async_trait};
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
use tracing::{debug, error, info, trace, warn};

use crate::ControllerCommand;

pub const UNEXPECTED_DISCONNECT: &str = "UNEXPECTED_DISCONNECT";

/// ----------------------------
/// Control channel protocol
/// ----------------------------
/// Wire is length-prefixed JSON for easy debugging:
/// [u32 BE length][ JSON bytes ]
/// ----------------------------

/// ----------------------------
/// ControlClient actor
/// Connects to controller, does handshake, receives JSON ControllerCommand,
/// emits AgentEvent through the OutputPort provided at construction.
/// ----------------------------
pub struct ControlClient;

pub enum ControlClientMsg {
    // internal messages
    SendCommand(ControllerCommand),
    // external queries (optional)
    // GetSessionId(RpcReplyPort<Option<String>>),
}
#[derive(Clone, Debug)]
pub enum ClientEvent {
    TransportError { reason: String },
    Connected,
    CommandReceived { command: ControllerCommand },
}
/// Actor state for the TCP client
pub struct ControlClientState {
    bind_addr: String,
    server_name: String,
    writer: Option<Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>, // Use Option to allow taking ownership
    client_config: Arc<ClientConfig>,
    /// Output port where message payloads and the topic it came from is piped to a dispatcher
    events_output: Arc<OutputPort<ClientEvent>>,
    abort_handle: Option<AbortHandle>, // abort handle switch for the stream read loop
}

/// A basse configuration for a TCP Client actor
pub struct ControlClientConfig {
    pub controller_addr: String,
    pub server_name: String,
    pub ca_certificate_path: String,
    pub client_certificate_path: String,
    pub client_key_path: String,
}

impl ControlClientConfig {
    /// Read filepaths from the environment and return. If we can't read these, we can't start
    pub fn new() -> Result<Self, ActorProcessingErr> {
        let client_certificate_path = env::var("TLS_CLIENT_CERT")
            .map_err(|_| error!("TLS_CLIENT_CERT not set"))
            .unwrap();
        let client_key_path = env::var("TLS_CLIENT_KEY")
            .map_err(|_| error!("TLS_CLIENT_KEY not set"))
            .unwrap();
        let ca_certificate_path = env::var("TLS_CA_CERT")
            .map_err(|_| error!("TLS_CA_CERT not set"))
            .unwrap();
        let controller_addr = env::var("CONTROLLER_ADDR")
            .map_err(|_| error!("CONTROLLER_ADDR not set"))
            .unwrap();
        let server_name = env::var("HARNESS_SERVER_NAME")
            .map_err(|_| error!("HARNESS_SERVER_NAME not set"))
            .unwrap();

        Ok(ControlClientConfig {
            controller_addr,
            server_name,
            ca_certificate_path,
            client_certificate_path,
            client_key_path,
        })
    }
}

pub struct ControlClientArgs {
    pub config: ControlClientConfig,
    pub events_output: Arc<OutputPort<ClientEvent>>,
}

impl ControlClient {
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
    /// helper to write a length-prefixed JSON message to the writer
    async fn write(
        state: &mut ControlClientState,
        command: ControllerCommand,
    ) -> Result<(), ActorProcessingErr> {
        match rkyv::to_bytes::<Error>(&command) {
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

    fn spawn_reader(
        &self,
        _myself: ActorRef<ControlClientMsg>,
        state: &mut ControlClientState,
    ) -> Result<(), ActorProcessingErr> {
        let reader = tokio::io::BufReader::new(state.reader.take().expect("Reader already taken"));

        let queue_out = state.events_output.clone();

        let abort = tokio::spawn(async move {
            let mut buf_reader = tokio::io::BufReader::new(reader);

            loop {
                match buf_reader.read_u32().await {
                    Ok(incoming_msg_length) => {
                        if incoming_msg_length > 0 {
                            let mut buffer = vec![0; incoming_msg_length as usize];

                            trace!("Reading {incoming_msg_length} byte(s).");
                            if let Ok(_) = buf_reader.read_exact(&mut buffer).await {
                                trace!(
                                    len = incoming_msg_length,
                                    first_16 = ?&buffer[..buffer.len().min(16)],
                                    "Received frame"
                                );

                                match rkyv::from_bytes::<ControllerCommand, rkyv::rancor::Error>(
                                    &buffer,
                                ) {
                                    Ok(command) => {
                                        // pipe up to supervisor
                                        queue_out.send(ClientEvent::CommandReceived { command });
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
                        info!("TCP reader terminating: {e}");
                        queue_out.send(ClientEvent::TransportError {
                            reason: format!("{UNEXPECTED_DISCONNECT} {e}"),
                        });
                        break;
                    }
                }
            }
        })
        .abort_handle();

        state.abort_handle = Some(abort);
        Ok(())
    }
}

#[async_trait]
impl Actor for ControlClient {
    type Msg = ControlClientMsg;
    type State = ControlClientState;
    type Arguments = ControlClientArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: ControlClientArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        let _span = tracing::info_span!("cassini.tcp-client.init").entered();
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

        // --- 7. Construct final state ---
        let state = ControlClientState {
            bind_addr: args.config.controller_addr,
            server_name: args.config.server_name,
            reader: None,
            writer: None,
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
        info!("Connecting to {addr}");

        let tls_stream = Self::connect_with_backoff(
            addr.clone(),
            state.server_name.clone(),
            &state.client_config,
        )
        .await?;
        let (reader, write_half) = split(tls_stream);
        let writer = tokio::io::BufWriter::new(write_half);

        state.reader = Some(reader);
        state.writer = Some(Arc::new(Mutex::new(writer)));

        info!("Successfully connected to {addr}. Listening for messages.");

        self.spawn_reader(myself.clone(), state)?;

        // tell supervisor we're connected.
        state.events_output.send(ClientEvent::Connected);
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            ControlClientMsg::SendCommand(command) => {
                Self::write(state, command).await?;
            }
        }
        Ok(())
    }
}
// ///
// /// A base configuration for a TCP Client actor
// pub struct HarnessClientConfig {
//     pub sink_endpoint: String,
//     pub server_name: String,
//     pub ca_certificate_path: String,
//     pub client_certificate_path: String,
//     pub client_key_path: String,
// }

// impl HarnessClientConfig {
//     /// Read filepaths from the environment and return. If we can't read these, we can't start
//     pub fn new() -> Self {
//         let client_certificate_path =
//             env::var("TLS_CLIENT_CERT").expect("Expected a value for TLS_CLIENT_CERT.");
//         let client_key_path =
//             env::var("TLS_CLIENT_KEY").expect("Expected a value for TLS_CLIENT_KEY.");
//         let ca_certificate_path =
//             env::var("TLS_CA_CERT").expect("Expected a value for TLS_CA_CERT.");
//         let sink_endpoint =
//             env::var("SINK_ADDR").expect("Expected a valid socket address for SINK_ADDR");
//         let server_name =
//             env::var("SINK_SERVER_NAME").expect("Expected a value for SINK_SERVER_NAME");

//         HarnessClientConfig {
//             sink_endpoint,
//             server_name,
//             ca_certificate_path,
//             client_certificate_path,
//             client_key_path,
//         }
//     }
// }

// pub struct HarnessClientArgs {
//     pub config: HarnessClientConfig,
// }

// /// Actor state for the TCP client
// pub struct HarnessClientState {
//     bind_addr: String,
//     server_name: String,
//     writer: Option<Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>>,
//     reader: Option<ReadHalf<TlsStream<TcpStream>>>, // Use Option to allow taking ownership
//     // output_port: Arc<OutputPort<HarnessControllerMessage>>,
//     abort_handle: Option<AbortHandle>, // abort handle switch for the stream read loop
//     client_config: Arc<ClientConfig>,  // tls client configuration
// }

// /// Messages handled by the TCP client actor
// #[derive(Debug)]
// pub enum HarnessClientMessage {
//     Send(ControllerCommand),
//     // Disconnect,
// }

// /// TCP client actor
// pub struct HarnessClient;

// impl HarnessClient {
//     /// Send a payload to the sink over the wire.
//     pub async fn send_message(
//         message: HarnessControllerMessage,
//         state: &mut HarnessClientState,
//     ) -> Result<(), Box<dyn std::error::Error>> {
//         match rkyv::to_bytes::<Error>(&message) {
//             Ok(bytes) => {
//                 //create new buffer
//                 let mut buffer = Vec::new();

//                 //get message length as header
//                 let len = (bytes.len() as u64).to_be_bytes();
//                 buffer.extend_from_slice(&len);

//                 //add message to buffer
//                 buffer.extend_from_slice(&bytes);

//                 //write message
//                 let unwrapped_writer = state.writer.clone().unwrap();
//                 let mut writer = unwrapped_writer.lock().await;
//                 if let Err(e) = writer.write_all(&buffer).await {
//                     error!("Failed to flush stream {e}");
//                     return Err(Box::new(e));
//                 }

//                 if let Err(e) = writer.flush().await {
//                     error!("Failed to flush stream {e}");
//                     return Err(Box::new(e));
//                 }

//                 Ok(())
//             }
//             Err(e) => {
//                 warn!("Failed to serialize message. {e}");
//                 Err(Box::new(e))
//             }
//         }
//     }
// }

// #[async_trait]
// impl Actor for HarnessClient {
//     type Msg = HarnessClientMessage;
//     type State = HarnessClientState;
//     type Arguments = HarnessClientArgs;

//     async fn pre_start(
//         &self,
//         _: ActorRef<Self::Msg>,
//         args: HarnessClientArgs,
//     ) -> Result<Self::State, ActorProcessingErr> {
//         info!("Starting TCP Client ");
//         // install default crypto provider
//         let provider = rustls::crypto::aws_lc_rs::default_provider().install_default();
//         if let Err(_) = provider {
//             debug!("Crypto provider configured");
//         }

//         let mut root_cert_store = rustls::RootCertStore::empty();
//         let _ = root_cert_store.add(
//             CertificateDer::from_pem_file(args.config.ca_certificate_path.clone()).expect(
//                 &format!(
//                     "Expected to read CA cert as a PEM file from {}",
//                     args.config.ca_certificate_path
//                 ),
//             ),
//         );

//         let client_cert = CertificateDer::from_pem_file(
//             args.config.client_certificate_path.clone(),
//         )
//         .expect(&format!(
//             "Expected to read client cert as a PEM file from {}",
//             args.config.client_certificate_path
//         ));
//         let private_key =
//             PrivateKeyDer::from_pem_file(args.config.client_key_path.clone()).expect(&format!(
//                 "Expected to read client key as a PEM file from {}",
//                 args.config.client_certificate_path
//             ));
//         let verifier = WebPkiServerVerifier::builder(Arc::new(root_cert_store))
//             .build()
//             .expect("Expected to build client verifier");
//         let mut certs = Vec::new();
//         certs.push(client_cert);

//         let config = rustls::ClientConfig::builder()
//             .with_webpki_verifier(verifier)
//             .with_client_auth_cert(certs, private_key)
//             .unwrap();

//         let state = HarnessClientState {
//             bind_addr: args.config.sink_endpoint,
//             server_name: args.config.server_name,
//             reader: None,
//             writer: None,
//             client_config: Arc::new(config),
//             abort_handle: None,
//         };

//         Ok(state)
//     }

//     async fn post_start(
//         &self,
//         myself: ActorRef<Self::Msg>,
//         state: &mut Self::State,
//     ) -> Result<(), ActorProcessingErr> {
//         let addr = state.bind_addr.clone();
//         info!("Connecting to {addr}");
//         let connector = TlsConnector::from(Arc::clone(&state.client_config));

//         match TcpStream::connect(&addr).await {
//             Ok(tcp_stream) => {
//                 let domain =
//                     ServerName::try_from(state.server_name.clone()).expect("invalid DNS name");

//                 match connector.connect(domain, tcp_stream).await {
//                     Ok(tls_stream) => {
//                         let (reader, write_half) = split(tls_stream);

//                         let writer = tokio::io::BufWriter::new(write_half);

//                         state.reader = Some(reader);
//                         state.writer = Some(Arc::new(Mutex::new(writer)));

//                         info!("{myself:?} Listening... ");
//                         let reader = tokio::io::BufReader::new(
//                             state.reader.take().expect("Reader already taken!"),
//                         );

//                         //start listening
//                         let abort_handle = tokio::spawn(async move {
//                             let mut buf_reader = tokio::io::BufReader::new(reader);

//                             while let Ok(incoming_msg_length) = buf_reader.read_u32().await {
//                                 if incoming_msg_length > 0 {
//                                     let mut buffer = vec![0; incoming_msg_length as usize];
//                                     if let Ok(_) = buf_reader.read_exact(&mut buffer).await {

//                                         match rkyv::access::<ArchivedHarnessControllerMessage, Error>(&buffer[..]) {
//                                             Ok(archived) => {
//                                                 // deserialize back to the original type
//                                                 if let Ok(deserialized) =
//                                                     deserialize::<HarnessControllerMessage, Error>(archived)
//                                                 {
//                                                     myself.try_get_supervisor().map(|service| {
//                                                         service
//                                                             .send_message(deserialized)
//                                                             .expect("Expected to forward command upwards");
//                                                     });
//                                                 }
//                                             }
//                                             Err(e) => {
//                                                 warn!("Failed to parse message: {e}");
//                                             }
//                                         }
//                                     }
//                                 }
//                             }
//                         })
//                         .abort_handle();

//                         state.abort_handle = Some(abort_handle);
//                     }
//                     Err(e) => {
//                         error!("Failed to establish mTLS connection! {e}");
//                         myself.stop(Some("Failed to establish mTLS connection! {e}".to_string()));
//                     }
//                 }
//             }
//             Err(e) => {
//                 error!("Failed to connect to server: {e}");
//                 // If we failed to connect to the sink, we should just stop.
//                 myself.notify_supervisor(ractor::SupervisionEvent::ActorFailed(
//                     myself.get_cell(),
//                     ActorProcessingErr::from(e),
//                 ));
//                 myself.stop(None);
//             }
//         };
//         Ok(())
//     }

//     async fn post_stop(
//         &self,
//         myself: ActorRef<Self::Msg>,
//         _: &mut Self::State,
//     ) -> Result<(), ActorProcessingErr> {
//         debug!("Successfully stopped {myself:?}");
//         Ok(())
//     }

//     async fn handle(
//         &self,
//         _myself: ActorRef<Self::Msg>,
//         message: Self::Msg,
//         state: &mut Self::State,
//     ) -> Result<(), ActorProcessingErr> {
//         match message {
//             HarnessClientMessage::Send(command) => {
//                 HarnessClient::send_message(command, state)
//                     .await
//                     .expect("Expected to send message successfully");
//             }
//         }
//         Ok(())
//     }
// }
