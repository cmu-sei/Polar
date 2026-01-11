use std::sync::Arc;
use std::{process::Stdio, time::Duration};
use harness_common::{ArchivedHarnessControllerMessage, HarnessControllerMessage, TestPlan};
use ractor::{
    async_trait, concurrency::JoinHandle, Actor, ActorProcessingErr, OutputPort,ActorRef, SupervisionEvent,
};

use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs};
use rkyv::{
    deserialize,
    rancor::{self, Error, Source},
};
use rustls::{
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
    server::WebPkiClientVerifier,
    RootCertStore, ServerConfig,
};

use tokio::process::Command;
use tokio::{
    io::{split, AsyncReadExt, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task::AbortHandle,
};
use tokio_rustls::{server::TlsStream, TlsAcceptor};
use tracing::{debug, error, info, warn};

pub const SINK_CLIENT_SESSION: &str = "cassini.harness.session";

// ============================== Sink Service ============================== //

pub struct HarnessController;

impl HarnessController {

    // --- Orchestration helper ---

    /// Spawn a TcpClientActor
    pub async fn start_client(
        myself: ActorRef<HarnessControllerMessage>,
        client_name: &str,
    ) -> Result<(), ActorProcessingErr> {
        // Create the output ports the client expects
        let events_output: Arc<OutputPort<String>> = Arc::new(OutputPort::default());
        let queue_output: Arc<OutputPort<(Vec<u8>, String)>> = Arc::new(OutputPort::default());

        //subscribe to events.
        events_output.subscribe(myself.clone(), |_registration_id: String| {
            info!("Successfully registered with Cassini.");
            Some(HarnessControllerMessage::Initialize)
        });

        let config = TCPClientConfig::new()?;
        // Prepare TcpClientArgs and spawn the client actor
        let tcp_args = TcpClientArgs {
            config,
            registration_id: None,
            events_output: events_output.clone(),
            queue_output: queue_output.clone(),
        };

        match Actor::spawn_linked(
            Some(format!("{client_name}-tcp-client")),
            TcpClientActor,
            tcp_args,
            myself.clone().into()
        )
        .await
        {
            Ok((_client_ref, _)) => Ok(()),
            Err(e) => Err(ActorProcessingErr::from(e))
        }
    }
}

pub struct HarnessControllerState {
    bind_addr: String,
    server_config: ServerConfig,
    pub test_plan: TestPlan,
    server_handle: Option<AbortHandle>,
    broker: Option<AbortHandle>,
    producer: Option<AbortHandle>,
    sink: Option<AbortHandle>,
    sink_path: String,
    broker_path: String,
    producer_path: String,
}

pub struct HarnessControllerArgs {
    pub bind_addr: String,
    pub server_cert_file: String,
    pub private_key_file: String,
    pub ca_cert_file: String,
    pub test_plan: TestPlan,
    pub sink_path: String,
    pub broker_path: String,
    pub producer_path: String,
}

#[async_trait]
impl Actor for HarnessController {
    type Msg = HarnessControllerMessage;
    type State = HarnessControllerState;
    type Arguments = HarnessControllerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: HarnessControllerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!("HarnessController: Starting {myself:?}");

        // install default crypto provider
        let provider = rustls::crypto::aws_lc_rs::default_provider().install_default();
        if let Err(_) = provider {
            debug!("Crypto provider configured");
        }

        tracing::debug!("HarnessController: Gathering certificates for mTLS");
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

        tracing::debug!("HarnessController: Building configuration for mTLS ");

        let server_config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, private_key)
            .expect("bad certificate/key");

        //set up state object
        let state = HarnessControllerState {
            bind_addr: args.bind_addr,
            server_config,
            test_plan: args.test_plan,
            server_handle: None,
            broker: None,
            producer: None,
            sink: None,
            sink_path: args.sink_path,
            broker_path: args.broker_path,
            producer_path: args.producer_path,
        };

        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} started.");

        let acceptor = TlsAcceptor::from(Arc::new(state.server_config.clone()));

        let server = TcpListener::bind(state.bind_addr.clone())
            .await
            .expect("could not start tcp listener");

        info!("HarnessController: Server running on {}", state.bind_addr);

        let plan = state.test_plan.clone();
        let cloned_self = myself.clone();

        let server_handle = tokio::spawn(async move {
            while let Ok((stream, peer_addr)) = server.accept().await {
                match acceptor.accept(stream).await {
                    Ok(stream) => {
                        let client_id = uuid::Uuid::new_v4().to_string();

                        let (reader, writer) = split(stream);

                        let writer = tokio::io::BufWriter::new(writer);

                        debug!("New connection from {peer_addr}");

                        let listener_args = ClientSessionArguments {
                            writer: Arc::new(Mutex::new(writer)),
                            reader: Some(reader),
                            // client_id: client_id.clone(),
                        };

                        //start listener actor to handle connection
                        let (client, _) = Actor::spawn_linked(
                            Some(client_id),
                            ClientSession,
                            listener_args,
                            cloned_self.clone().into(),
                        )
                        .await
                        .expect("Failed to start listener for new connection");

                        // send test plan
                        client
                            .cast(ClientSessionMessage::Send(
                                HarnessControllerMessage::TestPlan { plan: plan.clone() },
                            ))
                            .ok();
                    }
                    Err(e) => {
                        //We probably got pinged or something, ignore but log the attempt.
                        tracing::warn!("TLS handshake failed from {}: {}", peer_addr, e);
                    }
                }
            }
        })
        .abort_handle();

        state.server_handle = Some(server_handle);

        tracing::info!("Starting cassini server...");
        let broker = crate::spawn_broker(myself.clone(), state.broker_path.clone()).await;
        state.broker = Some(broker.clone());

        // Check readiness of the broker. Here's where we should open a connection and await registration to complete.
        match HarnessController::start_client(myself.clone(), "harness.controller.tcp").await {
            Ok(_) => Ok(()),
            Err(e) => {
                error!("Failed to register with the message broker. {e}");
                if let Some(handle) = &state.server_handle {
                    handle.abort();

                }
                Err(e)
            }
        }
    }

    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        //shutdown processes
        state.server_handle.as_ref().map(|handle| handle.abort());

        state.broker.as_ref().map(|handle| handle.abort());

        debug!("{myself:?} stopped");

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
            SupervisionEvent::ActorTerminated(actor_cell, _boxed_state, reason) => {
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
                error!(
                    "Worker agent: {0:?}:{1:?} failed!",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                todo!("When this happens, we should shut everything down gracefully.");
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
        debug!("{myself:?} Received a message");

        match message {
            HarnessControllerMessage::Initialize => {
                info!("Initializng test agents");
                // binaries are currently named aptly.
                // harness-sink
                // harness-producer
                // I guess there's not much sense in writing two fns.
                // TODO: add a handshake, and when it responds as healthy, we continue the test
                let cloned_self = myself.clone();
                let cloned_path = state.producer_path.clone();

                let producer = tokio::spawn(async move {
                    tracing::info!("Spawning producer service at {cloned_path}");
                    let mut child = Command::new(cloned_path)
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .spawn()
                        .expect("failed to spawn harness");

                    let status = child.wait().await.expect("harness {binary} crashed");
                    tracing::warn!(?status, "Harness producer exited");

                    let _ = cloned_self.cast(HarnessControllerMessage::Error {
                        reason: "harness {binary} crashed".to_string(),
                    });
                })
                .abort_handle();

                let cloned_self = myself.clone();
                let cloned_path = state.sink_path.clone();

                // TODO: add a handshake, and when it responds as healthy, we continue the test
                let sink = tokio::spawn(async move {
                    tracing::info!("Spawning producer service at {cloned_path}");
                    let mut child = Command::new(cloned_path)
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .spawn()
                        .expect("failed to spawn harness");

                    let status = child.wait().await.expect("harness {binary} crashed");
                    tracing::warn!(?status, "Harness sink exited");

                    let _ = cloned_self.cast(HarnessControllerMessage::Error {
                        reason: "harness {binary} crashed".to_string(),
                    });
                });
            }
            HarnessControllerMessage::ShutdownAck => myself.stop(Some("TEST_COMPLETE".to_string())),
            _ => (),
        }

        Ok(())
    }
}

struct ClientSession;

enum ClientSessionMessage {
    Send(HarnessControllerMessage),
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
        message: HarnessControllerMessage,
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
        let cloned_self = myself.clone();

        //start listening
        debug!("client listening...");
        let handle = tokio::spawn(async move {
            let mut buf_reader = tokio::io::BufReader::new(reader);
            // parse incoming message length, this tells us what size of a message to expect.
            while let Ok(incoming_msg_length) = buf_reader.read_u64().await {
                if incoming_msg_length > 0 {
                    // create a buffer of exact size, and read data in.
                    let mut buffer = vec![0u8; incoming_msg_length as usize];
                    if let Ok(_) = buf_reader.read_exact(&mut buffer).await {
                        // use unsafe API for maximum performance
                        match rkyv::access::<ArchivedHarnessControllerMessage, Error>(&buffer[..]) {
                            Ok(archived) => {
                                // deserialize back to the original type
                                if let Ok(deserialized) =
                                    deserialize::<HarnessControllerMessage, Error>(archived)
                                {
                                    cloned_self.try_get_supervisor().map(|service| {
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

        // // send client service its client_id
        // let client_id = myself
        //     .get_name()
        //     .expect("Expected client session to be named");

        // debug!("issuing client id.");
        // ClientSession::write(
        //     HarnessControllerMessage::ClientRegistered(client_id),
        //     state.writer.clone(),
        // )
        // .await
        // .ok();

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

// #[tokio::main]
// async fn main() {
//     use std::env;
//     polar::init_logging();

//     let server_cert_file = env::var("TLS_SERVER_CERT_CHAIN")
//         .expect("Expected a value for the TLS_SERVER_CERT_CHAIN environment variable.");
//     let private_key_file = env::var("TLS_SERVER_KEY")
//         .expect("Expected a value for the TLS_SERVER_KEY environment variable.");
//     let ca_cert_file = env::var("TLS_CA_CERT")
//         .expect("Expected a value for the TLS_CA_CERT environment variable.");
//     let bind_addr = env::var("SINK_BIND_ADDR").unwrap_or(String::from("0.0.0.0:3000"));

//     let args = HarnessControllerArgs {
//         bind_addr,
//         server_cert_file,
//         private_key_file,
//         ca_cert_file,
//     };

//     let (_, handle) = Actor::spawn(
//         Some("cassini.harness.sink.svc".to_string()),
//         HarnessController,
//         args,
//     )
//     .await
//     .expect("Expected to start sink server");

//     handle.await.unwrap();
// }
