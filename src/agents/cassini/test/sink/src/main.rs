use harness_common::{ArchivedSinkCommand, ProducerMessage, SinkCommand};
use harness_sink::sink::*;
use ractor::{
    Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait,
    concurrency::JoinHandle, registry::where_is,
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
use std::{process::Output, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf, split},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task::AbortHandle,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

pub const SINK_CLIENT_SESSION: &str = "cassini.harness.sink.session";

// ============================== Sink Service ============================== //

pub struct SinkService;

pub struct SinkServiceState {
    server_handle: AbortHandle,
    timeout_token: CancellationToken,
}

pub struct SinkServiceArgs {
    pub bind_addr: String,
    pub server_cert_file: String,
    pub private_key_file: String,
    pub ca_cert_file: String,
}

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

        let bind_addr = args.bind_addr.clone();
        let acceptor = TlsAcceptor::from(Arc::new(server_config));

        let server = TcpListener::bind(bind_addr.clone())
            .await
            .expect("could not start tcp listener");

        info!("SinkService: Server running on {bind_addr}");

        let server_handle = tokio::spawn(async move {
            while let Ok((stream, peer_addr)) = server.accept().await {
                match acceptor.accept(stream).await {
                    Ok(stream) => {
                        // let client_id = uuid::Uuid::new_v4().to_string();

                        let (reader, writer) = split(stream);

                        let writer = tokio::io::BufWriter::new(writer);

                        debug!("New connection from {peer_addr}");

                        let listener_args = ClientSessionArguments {
                            writer: Arc::new(Mutex::new(writer)),
                            reader: Some(reader),
                            // client_id: client_id.clone(),
                        };

                        //start listener actor to handle connection
                        let _ = Actor::spawn_linked(
                            Some(SINK_CLIENT_SESSION.to_string()),
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
        })
        .abort_handle();

        //set up state object
        let state = SinkServiceState {
            server_handle,
            timeout_token: CancellationToken::new(),
        };

        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // await contact from producer service
        // timeout after configured duration if no contact established

        // TODO: Make configurable
        let timeout = 30u64;
        let token = state.timeout_token.clone();
        let server_handle = state.server_handle.clone();
        let _ = tokio::spawn(async move {
            info!("Waiting {timeout} secs for contact from test client.");
            tokio::select! {
                // Use cloned token to listen to cancellation requests
                _ = token.cancelled() => {
                    info!("Cancelling timeout.")
                }
                // wait
                _ = tokio::time::sleep(std::time::Duration::from_secs(timeout)) => {
                    error!("Failed to receive contact from test harness. Shutting down.");
                    server_handle.abort();
                    myself.stop(Some("TEST_TIMED_OUT".to_string()));

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
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SinkCommand::TestPlan(plan) => {
                // abort timeout
                state.timeout_token.cancel();

                info!(
                    "Received test plan from client. {}",
                    serde_json::to_string_pretty(&plan).unwrap()
                );
                info!("Starting subscribes.");
                // start a sink for each producer, if we already have a sink subscriber for that topic, skip

                for config in plan.producers {
                    let args = SinkConfig {
                        topic: config.topic.clone(),
                    };

                    let _ = Actor::spawn_linked(
                        Some(format!("cassini.harness.sink.{}", config.topic)),
                        SinkAgent,
                        args,
                        myself.clone().into(),
                    )
                    .await
                    .map_err(|_e| ());
                }
            }
            SinkCommand::Stop => {
                info!("Shutdown command received. Shutting down.");

                // write back to the producer that we're stopping
                where_is(SINK_CLIENT_SESSION.to_string()).map(|session| {
                    session
                        .send_message(ClientSessionMessage::Send(ProducerMessage::ShutdownAck))
                        .expect("Expected to send message to client session")
                });

                state.server_handle.abort();
                myself.stop_children_and_wait(None, None).await;

                myself.stop(None);
            }
            _ => (),
        }

        Ok(())
    }
}

struct ClientSession;

enum ClientSessionMessage {
    Send(ProducerMessage),
}

struct ClientSessionState {
    writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    task_handle: Option<JoinHandle<()>>,
    // output_port: Arc<OutputPort<SinkCommand>>,
}

struct ClientSessionArguments {
    writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    // output_port: Arc<OutputPort<SinkCommand>>,
}

impl ClientSession {
    /// Helper fn to write messages to client
    ///
    /// writing binary data over the wire requires us to be specific about what we expect on the other side. So we note the length of the message
    /// we intend to send and write the amount to the *front* of the buffer to be read first, then write the actual message data to the buffer to be deserialized.
    ///
    async fn write(
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
                        warn!("Failed to send message to client {e}");
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
    type Msg = ClientSessionMessage;
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
            task_handle: None,
            // output_port: args.output_port,
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
        });

        // add join handle to state, we'll need to cancel it if a client disconnects
        state.task_handle = Some(handle);

        Ok(())
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ClientSessionMessage::Send(message) => {
                ClientSession::write(message, Arc::clone(&state.writer))
                    .await
                    .expect("expected to write message to client.");
            }
        }
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
