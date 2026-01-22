use crate::{Test, TestPlan};

use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent;
use harness_common::{AgentRole, ControllerCommand};
use ractor::{
    async_trait, concurrency::JoinHandle, Actor, ActorProcessingErr, ActorRef, OutputPort,
    SupervisionEvent,
};
use rkyv::rancor::Error;
use rkyv::rancor::Source;
use rustls::{
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
    server::WebPkiClientVerifier,
    RootCertStore, ServerConfig,
};
use std::sync::Arc;
use std::{process::Stdio, time::Duration};
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

struct TestState {
    producer: Option<ActorRef<ClientSessionMessage>>,
    producer_complete: bool,

    sink: Option<ActorRef<ClientSessionMessage>>,
    sink_complete: bool,
    // TODO: metrics
}

#[derive(Clone)]
pub enum HarnessControllerMessage {
    ClientConnected {
        client_id: String,
        client: ActorRef<ClientSessionMessage>, // peer_addr: String,
    },
    TestEvent(TestEvent),
    CassiniEvent(ClientEvent),
    StartTest,
    AdvanceTest,
}

pub struct HarnessController;

impl HarnessController {
    /// Helper to run an instance of the broker in another thread.
    pub async fn spawn_process(binary_path: String) -> AbortHandle {
        return tokio::spawn(async move {
            tracing::info!("Spawning binary at {binary_path}...");
            let mut child = Command::new(binary_path)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("failed to spawn broker");

            let status = child.wait().await.expect("broker crashed");
        })
        .abort_handle();
    }

    /// Called once registration is observed; kicks off the plan execution
    fn start_test(
        &self,
        myself: &ActorRef<HarnessControllerMessage>,
    ) -> Result<(), ActorProcessingErr> {
        // Mark that we've started and tell the actor to process the first step.
        // We avoid blocking here — we just push a message to the actor's mailbox.
        myself
            .cast(HarnessControllerMessage::StartTest)
            .map_err(ActorProcessingErr::from)
    }

    /// --- Orchestration helper ---
    pub async fn start_client(
        myself: ActorRef<HarnessControllerMessage>,
        client_name: &str,
    ) -> Result<ActorRef<TcpClientMessage>, ActorProcessingErr> {
        // Create the output ports the client expects
        let events_output: Arc<OutputPort<ClientEvent>> = Arc::new(OutputPort::default());

        // Subscribe to events (registration strings, etc.)
        events_output.subscribe(myself.clone(), |event| {
            Some(HarnessControllerMessage::CassiniEvent(event))
        });

        let config = TCPClientConfig::new()?;
        // Prepare TcpClientArgs and spawn the client actor
        let tcp_args = TcpClientArgs {
            config,
            registration_id: None,
            events_output: events_output.clone(),
        };

        match Actor::spawn_linked(
            Some(format!("{client_name}-tcp-client")),
            TcpClientActor,
            tcp_args,
            myself.clone().into(),
        )
        .await
        {
            Ok((client_ref, _)) => Ok(client_ref),
            Err(e) => Err(ActorProcessingErr::from(e)),
        }
    }

    pub fn issue_producer_configuration(
        state: &mut HarnessControllerState,
    ) -> Result<(), ActorProcessingErr> {
        if let Some(test) = state.test_plan.tests.get(state.current_test_index) {
            match state.test_state.as_ref() {
                Some(test_state) => {
                    let command = ControllerCommand::ProducerConfig {
                        producer: test.producer.clone(),
                    };

                    if let Some(session) = test_state.producer.as_ref() {
                        debug!("Issuing new producer configuration");
                        Ok(session.cast(ClientSessionMessage::Send(command))?)
                    } else {
                        return Err(ActorProcessingErr::from("No producer session found"));
                    }
                }
                None => return Err(ActorProcessingErr::from("No test state found.".to_string())),
            }
        } else {
            error!("No test found for current index!");
            return Err(ActorProcessingErr::from("No test configured.".to_string()));
        }
    }

    pub fn issue_sink_configuration(
        state: &mut HarnessControllerState,
    ) -> Result<(), ActorProcessingErr> {
        if let Some(test) = state.test_plan.tests.get(state.current_test_index) {
            match state.test_state.as_ref() {
                Some(test_state) => {
                    if let Some(sink_session) = test_state.sink.as_ref() {
                        debug!("issuing new sink configuration");

                        let topic = test.producer.topic.clone();
                        let reply = ControllerCommand::SinkTopic { topic };

                        Ok(sink_session.cast(ClientSessionMessage::Send(reply))?)
                    } else {
                        error!("No sink session found");
                        Err(ActorProcessingErr::from("No sink session found"))
                    }
                }
                None => {
                    error!("No test state found");
                    Err(ActorProcessingErr::from("No test state found"))
                }
            }
        } else {
            error!("No test found for current index!");
            Err(ActorProcessingErr::from("No test configured.".to_string()))
        }
    }
}

pub struct HarnessControllerState {
    pub current_test_index: usize,
    started: bool,
    client_ref: Option<ActorRef<TcpClientMessage>>,
    bind_addr: String,
    server_config: ServerConfig,
    registration_id: Option<String>,
    pub current_test: Option<Test>,
    pub test_plan: TestPlan,
    test_state: Option<TestState>,
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

        let acceptor = TlsAcceptor::from(Arc::new(server_config.clone()));

        let server = TcpListener::bind(args.bind_addr.clone())
            .await
            .expect("could not start tcp listener");

        info!("HarnessController: Server running on {}", args.bind_addr);

        let cloned_self = myself.clone();
        let server_handle = tokio::spawn(async move {
            while let Ok((stream, peer_addr)) = server.accept().await {
                match acceptor.accept(stream).await {
                    Ok(stream) => {
                        let client_id = uuid::Uuid::new_v4().to_string();

                        let (reader, writer) = split(stream);

                        let writer = tokio::io::BufWriter::new(writer);

                        debug!("New connection from {peer_addr}");

                        let output_port = OutputPort::default();

                        // handle events such as transport errors in actor
                        output_port.subscribe(myself.clone(), |event| {
                            Some(HarnessControllerMessage::TestEvent(event))
                        });

                        let listener_args = ClientSessionArguments {
                            writer: Arc::new(Mutex::new(writer)),
                            reader: Some(reader),
                            client_id: client_id.clone(),
                            output_port,
                        };

                        //start listener actor to handle connection
                        let (client, _) = Actor::spawn_linked(
                            Some(client_id.clone()),
                            ClientSession,
                            listener_args,
                            cloned_self.clone().into(),
                        )
                        .await
                        .expect("Failed to start listener for new connection");

                        myself
                            .cast(HarnessControllerMessage::ClientConnected { client_id, client })
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

        //set up state object
        let state = HarnessControllerState {
            client_ref: None,
            started: false,
            current_test_index: 0,
            current_test: None,
            registration_id: None,
            bind_addr: args.bind_addr,
            server_config,
            test_plan: args.test_plan,
            server_handle: Some(server_handle),
            broker: None,
            producer: None,
            sink: None,
            test_state: None,
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

        tracing::info!("Starting cassini server...");
        let broker = HarnessController::spawn_process(state.broker_path.clone()).await;
        state.broker = Some(broker.clone());

        // Check readiness of the broker. Here's where we should open a connection and await registration to complete.
        match HarnessController::start_client(myself.clone(), "harness.controller.tcp").await {
            Ok(client_ref) => {
                state.client_ref = Some(client_ref);
                Ok(())
            }
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
        state.producer.as_ref().map(|handle| handle.abort());
        state.sink.as_ref().map(|handle| handle.abort());
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
            HarnessControllerMessage::CassiniEvent(event) => {
                match event {
                    ClientEvent::Registered { registration_id } => {
                        info!("Controller registered: {}", registration_id);

                        state.registration_id = Some(registration_id);

                        // Kick off the test now that we're registered
                        // This will queue StartTest and then the actor will process the first step.
                        myself.cast(HarnessControllerMessage::StartTest).ok();
                    }
                    ClientEvent::TransportError { reason } => {
                        error!("Client transport error: {}", reason);
                        // if transport fails, consider failing the test
                        return Err(ActorProcessingErr::from("Transport error"));
                    }
                    _ => (),
                }
            }
            HarnessControllerMessage::TestEvent(event) => match event {
                TestEvent::CommandReceived { reply_to, command } => match command {
                    ControllerCommand::Hello { role } => match role {
                        AgentRole::Producer => {
                            debug!("Received hello from producer");

                            if let Some(test) = state.test_plan.tests.get(state.current_test_index)
                            {
                                state
                                    .test_state
                                    .as_mut()
                                    .map(|state| state.producer = Some(reply_to.clone()));

                                let reply = ControllerCommand::ProducerConfig {
                                    producer: test.producer.clone(),
                                };

                                reply_to.cast(ClientSessionMessage::Send(reply)).ok();
                            } else {
                                error!("No test found for current index!");
                                return Err(ActorProcessingErr::from(
                                    "No test configured.".to_string(),
                                ));
                            }
                        }
                        AgentRole::Sink => {
                            debug!("Received hello from sink");
                            if let Some(test) = state.test_plan.tests.get(state.current_test_index)
                            {
                                state
                                    .test_state
                                    .as_mut()
                                    .map(|state| state.sink = Some(reply_to.clone()));

                                let topic = test.producer.topic.clone();
                                let reply = ControllerCommand::SinkTopic { topic };

                                reply_to.cast(ClientSessionMessage::Send(reply)).ok();
                            } else {
                                error!("No test found for current index!");
                                return Err(ActorProcessingErr::from(
                                    "No test configured.".to_string(),
                                ));
                            }
                        }
                    },
                    ControllerCommand::TestComplete { role, .. } => {
                        if let Some(test_state) = state.test_state.as_mut() {
                            match role {
                                AgentRole::Producer => {
                                    debug!("Producer agent completed task");
                                    test_state.producer_complete = true;
                                    // tell the sink to shutdown
                                    if let Some(sink_session) = test_state.sink.as_ref() {
                                        sink_session.send_message(ClientSessionMessage::Send(
                                            ControllerCommand::ProducerFinished,
                                        ))?;
                                    } else {
                                        todo!("if there's no sink, what do we do?")
                                    }
                                }
                                AgentRole::Sink => {
                                    debug!("Sink agent completed task");
                                    test_state.sink_complete = true;
                                }
                            }
                            // if test is finished, advance to next phase
                            // otherwise wait for the other to finish.
                            if test_state.producer_complete == true
                                && test_state.sink_complete == true
                            {
                                debug!("Test {} completed!", state.current_test_index);
                                myself.cast(HarnessControllerMessage::AdvanceTest).ok();
                            } else {
                                debug!("Waiting for other agent to complete");
                            }
                        } else {
                            error!("No test state found");
                            myself.stop(Some("No test state found".to_string()))
                        }
                    }
                    ControllerCommand::TestError { error } => {
                        error!("Test failed! {error}");
                        myself.stop(Some(error));
                    }
                    _ => warn!("Unexpected command! {command:?}"),
                },
                TestEvent::TransportError { client_id, reason } => {
                    error!("client {client_id} encountered an error {reason} stopping test");
                    return Err(ActorProcessingErr::from(reason));
                }
            },
            HarnessControllerMessage::StartTest => {
                if !state.started {
                    state.started = true;

                    // start sink
                    state.sink =
                        Some(HarnessController::spawn_process(state.sink_path.clone()).await);

                    // start producer
                    state.producer =
                        Some(HarnessController::spawn_process(state.producer_path.clone()).await);

                    // init test state, will be updated when agents handshake
                    state.test_state = Some(TestState {
                        producer: None,
                        sink: None,
                        producer_complete: false,
                        sink_complete: false,
                    });
                    // ensure index starts at 0
                    // state.current_test_index = 0;
                    // start processing
                    // schedule async evaluation of next step
                    // let _ = myself.cast(HarnessControllerMessage::AdvanceTest);
                } else {
                    error!("Received start directive during active test. Stopping.");
                    myself.stop(Some("Duplicate start directive".to_string()));
                }
            }

            HarnessControllerMessage::AdvanceTest => {
                //check that we can actually move forward

                state.current_test_index += 1;

                match state.test_plan.tests.get(state.current_test_index) {
                    Some(_test) => {
                        debug!("Advancing to next test {}", state.current_test_index);

                        // reset test state

                        if let Some(test_state) = state.test_state.as_mut() {
                            test_state.producer_complete = false;
                            test_state.sink_complete = false;
                        } else {
                            error!("No test state found");
                            myself.stop(Some("No test state found".to_string()));
                        }

                        Self::issue_sink_configuration(state)?;
                        Self::issue_producer_configuration(state)?;
                    }
                    None => {
                        // we're done
                        info!("Test completed");

                        state
                            .client_ref
                            .as_ref()
                            .map(|client| client.send_message(TcpClientMessage::Disconnect));

                        state.test_state.as_ref().map(|test_state| {
                            test_state.producer.as_ref().map(|session| {
                                session.send_message(ClientSessionMessage::Send(
                                    ControllerCommand::Shutdown,
                                ))
                            });
                        });

                        state.test_state.as_ref().map(|test_state| {
                            test_state.sink.as_ref().map(|session| {
                                session.send_message(ClientSessionMessage::Send(
                                    ControllerCommand::Shutdown,
                                ))
                            });
                        });

                        myself
                            .stop_and_wait(
                                Some("test complete".to_string()),
                                Some(Duration::from_millis(500)),
                            )
                            .await?;
                    }
                }
                // finally
                // turns out, with our current configuration, we can actually reuse the live agents.
                // So what we'll want to do is, incrememnt the phase ndex, and send out new configurations. By the time we get here
                // the agent's should have already stopped their children and disconnected from cassini,
                //  and be left in a waiting state until they're told to shutdown.

                // we need to do a few things within each phase
                // start sinks with their topics
                // start producers
                // wait until producers announce completion
                // wait until sinks announce completion of validation checks
                // advance to next phase
                // Reuse the broker instance between phases or restart
                // How do we wait for handshakes from producers?
                //

                //
            }
            HarnessControllerMessage::ClientConnected { client_id, .. } => {
                debug!("Client connected: {}", client_id);
            }
        }

        Ok(())
    }
}

struct ClientSession;

/// events that can occur related to the client session
#[derive(Clone)]
pub enum TestEvent {
    CommandReceived {
        reply_to: ActorRef<ClientSessionMessage>,
        command: ControllerCommand,
    },
    TransportError {
        client_id: String,
        reason: String,
    },
}

pub enum ClientSessionMessage {
    Send(ControllerCommand),
}

struct ClientSessionState {
    writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    task_handle: Option<JoinHandle<()>>,
    client_id: String,
    output_port: Arc<OutputPort<TestEvent>>,
}

struct ClientSessionArguments {
    writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    client_id: String,
    output_port: OutputPort<TestEvent>,
}

impl ClientSession {
    /// Helper fn to write messages to client
    ///
    /// writing binary data over the wire requires us to be specific about what we expect on the other side. So we note the length of the message
    /// we intend to send and write the amount to the *front* of the buffer to be read first, then write the actual message data to the buffer to be deserialized.
    ///
    async fn write(
        message: ControllerCommand,
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
                        return Err(Error::new(e));
                    }
                    Ok(if let Err(e) = writer.flush().await {
                        return Err(Error::new(e));
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
            client_id: args.client_id,
            output_port: Arc::new(args.output_port),
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
        let output_port = state.output_port.clone();
        let client_id = state.client_id.clone();
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
                        match rkyv::access::<
                            harness_common::ArchivedControllerCommand,
                            rkyv::rancor::Error,
                        >(&buffer[..])
                        {
                            Ok(archived) => {
                                // deserialize back to the original type
                                if let Ok(command) = rkyv::deserialize::<
                                    ControllerCommand,
                                    rkyv::rancor::Error,
                                >(archived)
                                {
                                    let message = HarnessControllerMessage::TestEvent(
                                        TestEvent::CommandReceived {
                                            reply_to: myself.clone(),
                                            command,
                                        },
                                    );

                                    cloned_self.try_get_supervisor().map(|service| {
                                        service
                                            .send_message(message)
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

            // TODO: emit an event alerting the server the connection was interrupted.
            output_port.send(TestEvent::TransportError {
                client_id,
                reason: "connection interrupted".to_string(),
            });
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
