use cassini_client::*;

use harness_common::{ArchivedSinkCommand, ProducerMessage, SinkCommand};
use ractor::{
    Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait, concurrency::JoinHandle,
};
use rkyv::{
    deserialize,
    rancor::{self, Error, Source},
};
use rustls::{
    RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    server::WebPkiClientVerifier,
};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf, split},
    net::{TcpListener, TcpStream},
    sync::Mutex,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tracing::{debug, error, info, warn};

// ============================== Sink Service ============================== //

pub struct SinkService;

pub struct SinkServiceState {
    bind_addr: String,
    server_config: Arc<ServerConfig>,
}

pub struct SinkServiceArgs {
    pub bind_addr: String,
    pub server_cert_file: String,
    pub private_key_file: String,
    pub ca_cert_file: String,
}

// pub enum SinkServiceMessage {
// }

#[async_trait]
impl Actor for SinkService {
    type Msg = SinkCommand;
    type State = SinkServiceState;
    type Arguments = SinkServiceArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: SinkServiceArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("SinkService: Starting {myself:?}");

        // install default crypto provider
        let provider = rustls::crypto::aws_lc_rs::default_provider().install_default();
        if let Err(_) = provider {
            debug!("Crypto provider configured");
        }

        tracing::debug!("SinkService: Gathering certificates for mTLS");
        let certs = CertificateDer::pem_file_iter(args.server_cert_file)
            .unwrap()
            .map(|cert| cert.unwrap())
            .collect();

        let mut root_store = RootCertStore::empty();
        let root_cert = CertificateDer::from_pem_file(args.ca_cert_file.clone()).expect(&format!(
            "Expected to read CA cert as a PEM file from {}",
            args.ca_cert_file
        ));
        root_store
            .add(root_cert)
            .expect("Expected to add root cert to server store");

        let verifier = WebPkiClientVerifier::builder(Arc::new(root_store))
            .build()
            .expect("Expected to build server verifier");

        let private_key =
            PrivateKeyDer::from_pem_file(args.private_key_file.clone()).expect(&format!(
                "Expected to load private key as a PEM file from {}",
                args.private_key_file
            ));

        tracing::debug!("SinkService: Building configuration for mTLS ");

        let server_config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, private_key)
            .expect("bad certificate/key");

        //set up state object
        let state = SinkServiceState {
            bind_addr: args.bind_addr,
            server_config: Arc::new(server_config),
        };
        info!("SinkService: Agent starting");
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let bind_addr = state.bind_addr.clone();
        let acceptor = TlsAcceptor::from(Arc::clone(&state.server_config));

        let server = TcpListener::bind(bind_addr.clone())
            .await
            .expect("could not start tcp listener");

        info!("SinkService: Server running on {bind_addr}");

        let _ = tokio::spawn(async move {
            while let Ok((stream, peer_addr)) = server.accept().await {
                match acceptor.accept(stream).await {
                    Ok(stream) => {
                        let client_id = uuid::Uuid::new_v4().to_string();

                        let (reader, writer) = split(stream);

                        let writer = tokio::io::BufWriter::new(writer);

                        debug!("New connection from {peer_addr}. Client ID: {client_id}");
                        let listener_args = ClientSessionArguments {
                            writer: Arc::new(Mutex::new(writer)),
                            reader: Some(reader),
                            client_id: client_id.clone(),
                        };

                        //start listener actor to handle connection
                        let _ = Actor::spawn_linked(
                            Some(client_id.clone()),
                            ClientSession,
                            listener_args,
                            myself.clone().into(),
                        )
                        .await
                        .expect("Failed to start listener for new connection");
                    }
                    Err(e) => {
                        //We probably got pinged or something, ignore but log the attempt.
                        tracing::warn!("TLS handshake failed from {}: {}", peer_addr, e);
                    }
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
                info!(
                    "Worker agent: {0:?}:{1:?} started",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorTerminated(actor_cell, boxed_state, reason) => {
                let client_id = actor_cell
                    .get_name()
                    .expect("Expected client listener to have been named");

                info!(
                    "Client listener: {0}:{1:?} stopped. {reason:?}",
                    client_id,
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorFailed(actor_cell, _) => {
                warn!(
                    "Worker agent: {0:?}:{1:?} failed!",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ProcessGroupChanged(_) => (),
        }

        Ok(())
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        todo!();

        Ok(())
    }
}

struct ClientSession;

struct ClientSessionState {
    writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    client_id: String,
    task_handle: Option<JoinHandle<()>>,
}

struct ClientSessionArguments {
    writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    client_id: String,
}

impl ClientSession {
    /// Helper fn to write messages to client
    ///
    /// writing binary data over the wire requires us to be specific about what we expect on the other side. So we note the length of the message
    /// we intend to send and write the amount to the *front* of the buffer to be read first, then write the actual message data to the buffer to be deserialized.
    ///
    async fn write(
        client_id: String,
        message: ProducerMessage,
        writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    ) -> Result<(), Error> {
        match rkyv::to_bytes::<Error>(&message) {
            Ok(bytes) => {
                //create new buffer
                let mut buffer = Vec::new();

                //get message length as header
                let len = bytes.len().to_be_bytes();
                buffer.extend_from_slice(&len);

                //add message to buffer
                buffer.extend_from_slice(&bytes);

                tokio::spawn(async move {
                    let mut writer = writer.lock().await;
                    if let Err(e) = writer.write_all(&buffer).await {
                        warn!("Failed to send message to client {client_id}: {e}");
                        return Err(rancor::Error::new(e));
                    }
                    Ok(if let Err(e) = writer.flush().await {
                        return Err(rancor::Error::new(e));
                    })
                })
                .await
                .expect("Expected write thread to finish")
            }
            Err(e) => Err(e),
        }
    }
}

#[async_trait]
impl Actor for ClientSession {
    type Msg = ();
    type State = ClientSessionState;
    type Arguments = ClientSessionArguments;

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        args: ClientSessionArguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(ClientSessionState {
            writer: args.writer,
            reader: args.reader,
            client_id: args.client_id.clone(),
            task_handle: None,
        })
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // stop listening
        //
        state.task_handle.as_ref().map(|handle| {
            handle.abort();
        });
        Ok(())
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let reader = state.reader.take().expect("Reader already taken!");

        // TODO: start a timer, if we're not contacted by the producer svc, we should consider it a failed test

        //start listening
        let handle = tokio::spawn(async move {
            let mut buf_reader = tokio::io::BufReader::new(reader);
            // parse incoming message length, this tells us what size of a message to expect.
            while let Ok(incoming_msg_length) = buf_reader.read_u64().await {
                if incoming_msg_length > 0 {
                    // create a buffer of exact size, and read data in.
                    let mut buffer = vec![0u8; incoming_msg_length as usize];
                    if let Ok(_) = buf_reader.read_exact(&mut buffer).await {
                        // use unsafe API for maximum performance
                        match rkyv::access::<ArchivedSinkCommand, Error>(&buffer[..]) {
                            Ok(archived) => {
                                // deserialize back to the original type
                                if let Ok(deserialized) =
                                    deserialize::<SinkCommand, Error>(archived)
                                {
                                    debug!("{deserialized:?}");
                                }
                            }
                            Err(e) => {
                                warn!("Failed to parse message: {e}");
                            }
                        }
                    }
                }
            }
        });

        // add join handle to state, we'll need to cancel it if a client disconnects
        state.task_handle = Some(handle);

        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    use std::env;
    polar::init_logging();

    let server_cert_file = env::var("TLS_SERVER_CERT_CHAIN")
        .expect("Expected a value for the TLS_SERVER_CERT_CHAIN environment variable.");
    let private_key_file = env::var("TLS_SERVER_KEY")
        .expect("Expected a value for the TLS_SERVER_KEY environment variable.");
    let ca_cert_file = env::var("TLS_CA_CERT")
        .expect("Expected a value for the TLS_CA_CERT environment variable.");
    let bind_addr = env::var("SINK_BIND_ADDR").unwrap_or(String::from("0.0.0.0:3000"));

    let args = SinkServiceArgs {
        bind_addr,
        server_cert_file,
        private_key_file,
        ca_cert_file,
    };

    let (_, handle) = Actor::spawn(
        Some("cassini.harness.sink.svc".to_string()),
        SinkService,
        args,
    )
    .await
    .expect("Expected to start sink server");

    handle.await.unwrap();
}
