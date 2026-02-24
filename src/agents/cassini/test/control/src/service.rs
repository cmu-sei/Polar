use crate::{Test, TestPlan};

use cassini_client::{
    ClientEventForwarder, ClientEventForwarderArgs, TCPClientConfig, TcpClientActor,
    TcpClientArgs, TcpClientMessage,
};
use cassini_types::{ClientEvent, ControlResult};
use harness_common::{AgentRole, ControllerCommand, MessagePattern};
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
use std::{collections::HashMap, env, process::Stdio, time::Duration};
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio::{
    io::{split, AsyncReadExt, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task::AbortHandle,
};
use tokio_rustls::{server::TlsStream, TlsAcceptor};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

pub const SINK_CLIENT_SESSION: &str = "cassini.harness.session";

struct TestState {
    producer: Option<ActorRef<ClientSessionMessage>>,
    producer_complete: bool,
    sink: Option<ActorRef<ClientSessionMessage>>,
    sink_complete: bool,
    sink_ready: bool,
}

#[derive(Clone)]
pub enum HarnessControllerMessage {
    ClientConnected {
        client_id: String,
        client: ActorRef<ClientSessionMessage>,
    },
    TestEvent(TestEvent),
    CassiniEvent(ClientEvent),
    StartTest,
    AdvanceTest,
    InitiateShutdown {
        auth_token: Option<String>,
        timeout: Option<Duration>,
    },
    BrokerShutdownComplete,
    ShutdownTimeout,
}

pub struct HarnessController;

impl HarnessController {
    async fn wait_for_broker_ready(
        broker_addr: &str,
        server_name: &str,
        timeout: Duration,
    ) -> Result<(), ActorProcessingErr> {
        use rustls::ClientConfig;
        use rustls::client::WebPkiServerVerifier;
        use rustls::pki_types::{CertificateDer, ServerName};
        use std::sync::Arc;
        use tokio::net::TcpStream;
        use tokio_rustls::TlsConnector;

        let ca_cert_path = env::var("TLS_CA_CERT")
            .map_err(|_| ActorProcessingErr::from("TLS_CA_CERT not set"))?;
        let client_cert_path = env::var("TLS_CLIENT_CERT")
            .map_err(|_| ActorProcessingErr::from("TLS_CLIENT_CERT not set"))?;
        let client_key_path = env::var("TLS_CLIENT_KEY")
            .map_err(|_| ActorProcessingErr::from("TLS_CLIENT_KEY not set"))?;

        let mut root_store = rustls::RootCertStore::empty();
        let ca_cert = CertificateDer::from_pem_file(&ca_cert_path)
            .map_err(|e| ActorProcessingErr::from(format!("Failed to read CA cert: {}", e)))?;
        root_store
            .add(ca_cert)
            .map_err(|_| ActorProcessingErr::from("Failed to add CA cert"))?;

        let verifier = WebPkiServerVerifier::builder(Arc::new(root_store))
            .build()
            .map_err(|e| ActorProcessingErr::from(format!("Failed to build verifier: {}", e)))?;

        let mut certs = Vec::new();
        let client_cert = CertificateDer::from_pem_file(&client_cert_path)
            .map_err(|e| ActorProcessingErr::from(format!("Failed to read client cert: {}", e)))?;
        certs.push(client_cert);

        let private_key = rustls::pki_types::PrivateKeyDer::from_pem_file(&client_key_path)
            .map_err(|e| ActorProcessingErr::from(format!("Failed to read client key: {}", e)))?;

        let client_config = ClientConfig::builder()
            .with_webpki_verifier(verifier)
            .with_client_auth_cert(certs, private_key)
            .map_err(|e| ActorProcessingErr::from(format!("Failed to build client config: {}", e)))?;

        let connector = TlsConnector::from(Arc::new(client_config));
        let server_name = ServerName::try_from(server_name.to_string())
            .map_err(|_| ActorProcessingErr::from("Invalid server name"))?;

        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Ok(tcp) = TcpStream::connect(broker_addr).await {
                if connector.connect(server_name.clone(), tcp).await.is_ok() {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err(ActorProcessingErr::from(format!(
            "Broker not ready after {:.1?}",
            timeout
        )))
    }

    pub async fn start_client(
        myself: ActorRef<HarnessControllerMessage>,
        client_name: &str,
    ) -> Result<ActorRef<TcpClientMessage>, ActorProcessingErr> {
        let (forwarder, _) = Actor::spawn_linked(
            Some(format!("{client_name}-event-forwarder")),
            ClientEventForwarder::new(),
            ClientEventForwarderArgs {
                target: myself.clone(),
                mapper: Box::new(|event| Some(HarnessControllerMessage::CassiniEvent(event))),
            },
            myself.clone().into(),
        )
        .await?;

        let config = TCPClientConfig::new()?;
        let tcp_args = TcpClientArgs {
            config,
            registration_id: None,
            events_output: None,
            event_handler: Some(forwarder.into()),
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
                        Err(ActorProcessingErr::from("No producer session found"))
                    }
                }
                None => Err(ActorProcessingErr::from("No test state found.".to_string())),
            }
        } else {
            error!("No test found for current index!");
            Err(ActorProcessingErr::from("No test configured.".to_string()))
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
                        let expected = match &test.producer.pattern {
                            MessagePattern::Drip { idle_time_seconds } => {
                                (test.producer.duration as f64 * *idle_time_seconds as f64).ceil() as usize
                            }
                            MessagePattern::Burst {
                                burst_size,
                                idle_time_seconds,
                            } => {
                                (test.producer.duration as f64 * *idle_time_seconds as f64).ceil() as usize * burst_size
                            }
                        };
                        let reply = ControllerCommand::SinkConfig {
                            topic,
                            expected_count: Some(expected),
                        };
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

    async fn send_shutdown_command(
        myself: ActorRef<HarnessControllerMessage>,
        state: &mut HarnessControllerState,
        auth_token: Option<String>,
    ) {
        info!("Initiating broker graceful shutdown...");
        if let Some(client_ref) = &state.client_ref {
            let shutdown_op = cassini_types::ControlOp::PrepareForShutdown {
                auth_token: auth_token.unwrap_or_default(),
            };

            if let Err(e) = client_ref.send_message(TcpClientMessage::ControlRequest {
                op: shutdown_op,
                trace_ctx: None,
            }) {
                warn!("Failed to send shutdown command via control channel ({}), will kill process directly", e);
                // Kill the process via kill channel
                if let Some(kill_tx) = state.broker_kill.take() {
                    let _ = kill_tx.send(());
                }
                if let Some(task) = state.broker_task.take() {
                    task.abort();
                }
                return;
            }

            info!("Shutdown command sent, waiting for confirmation...");
            state.waiting_for_shutdown_confirmation = true;

            let myself_clone = myself.clone();
            let handle = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let _ = myself_clone.send_message(HarnessControllerMessage::ShutdownTimeout);
            });
            state.shutdown_timeout = Some(handle.abort_handle());
        } else {
            warn!("Control client already dead, killing broker process directly");
            if let Some(kill_tx) = state.broker_kill.take() {
                let _ = kill_tx.send(());
            }
            if let Some(task) = state.broker_task.take() {
                task.abort();
            }
            myself.stop(Some("Shutdown via kill".to_string()));
        }
    }
}

pub struct HarnessControllerState {
    pub current_test_index: usize,
    started: bool,
    clients: HashMap<String, ActorRef<ClientSessionMessage>>,
    client_ref: Option<ActorRef<TcpClientMessage>>,
    registration_id: Option<String>,
    pub current_test: Option<Test>,
    pub test_plan: TestPlan,
    test_state: Option<TestState>,
    server_handle: Option<AbortHandle>,
    // Broker process handling
    broker_task: Option<JoinHandle<()>>,      // task waiting for broker exit
    broker_kill: Option<oneshot::Sender<()>>, // channel to kill broker
    producer: Option<JoinHandle<()>>,
    sink: Option<JoinHandle<()>>,
    sink_path: String,
    broker_path: String,
    producer_path: String,
    expected_message_count: Option<usize>,
    shutdown_initiated: bool,
    pub waiting_for_shutdown_confirmation: bool,
    pub shutdown_timeout: Option<tokio::task::AbortHandle>,
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

        let private_key = PrivateKeyDer::from_pem_file(args.private_key_file.clone()).expect(
            &format!(
                "Expected to load private key as a PEM file from {}",
                args.private_key_file
            ),
        );

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
                        output_port.subscribe(myself.clone(), |event| {
                            Some(HarnessControllerMessage::TestEvent(event))
                        });

                        let listener_args = ClientSessionArguments {
                            writer: Arc::new(Mutex::new(writer)),
                            reader: Some(reader),
                            client_id: client_id.clone(),
                            output_port,
                        };

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
                        tracing::warn!("TLS handshake failed from {}: {}", peer_addr, e);
                    }
                }
            }
        })
        .abort_handle();

        let state = HarnessControllerState {
            clients: HashMap::new(),
            client_ref: None,
            started: false,
            current_test_index: 0,
            current_test: None,
            registration_id: None,
            test_plan: args.test_plan,
            server_handle: Some(server_handle),
            broker_task: None,
            broker_kill: None,
            producer: None,
            sink: None,
            test_state: None,
            sink_path: args.sink_path,
            broker_path: args.broker_path,
            producer_path: args.producer_path,
            expected_message_count: None,
            shutdown_initiated: false,
            waiting_for_shutdown_confirmation: false,
            shutdown_timeout: None,
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
        let mut child = Command::new(&state.broker_path)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| ActorProcessingErr::from(format!("failed to spawn broker: {}", e)))?;

        let (kill_tx, mut kill_rx) = oneshot::channel();

        let task = tokio::spawn(async move {
            tokio::select! {
                status = child.wait() => {
                    info!("Broker process exited with status: {:?}", status);
                }
                _ = &mut kill_rx => {
                    info!("Killing broker process...");
                    let _ = child.kill().await;
                    let _ = child.wait().await; // reap
                }
            }
        });

        state.broker_task = Some(task);
        state.broker_kill = Some(kill_tx);

        let broker_addr = env::var("BROKER_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
        let server_name = env::var("CASSINI_SERVER_NAME").unwrap_or_else(|_| "localhost".to_string());
        info!("Waiting for broker mTLS at {} (SNI={})...", broker_addr, server_name);

        match Self::wait_for_broker_ready(&broker_addr, &server_name, Duration::from_secs(10)).await {
            Ok(()) => info!("Broker is ready."),
            Err(e) => {
                error!("Broker failed to become ready: {}", e);
                if let Some(kill_tx) = state.broker_kill.take() {
                    let _ = kill_tx.send(());
                }
                if let Some(task) = state.broker_task.take() {
                    task.abort();
                }
                return Err(e);
            }
        }

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
        debug!("HarnessController stopping, waiting for child actors to terminate...");

        let children = myself.get_children();
        for child in &children {
            let name = child.get_name().unwrap_or("<unnamed>".to_string());
            debug!("Stopping child {name}...");
            child.stop(Some("CONTROLLER_STOPPING".to_string()));
        }
        for child in children {
            let name = child.get_name().unwrap_or("<unnamed>".to_string());
            debug!("Waiting for child {name} to terminate...");
            if let Err(e) = child.wait(None).await {
                warn!("Child {name} terminated with error: {}", e);
            }
        }

        state.producer.as_ref().map(|handle| handle.abort());
        state.sink.as_ref().map(|handle| handle.abort());
        state.server_handle.as_ref().map(|handle| handle.abort());

        // Kill broker if still alive
        if let Some(kill_tx) = state.broker_kill.take() {
            let _ = kill_tx.send(());
        }
        if let Some(task) = state.broker_task.take() {
            task.abort();
        }

        debug!("HarnessController stopped");
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        state: &mut Self::State,
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
                let client_id = actor_cell.get_name().unwrap_or_default();
                info!(
                    "Client listener: {0}:{1:?} stopped. {reason:?}",
                    client_id, actor_cell.get_id()
                );
                if let Some(ref client) = state.client_ref {
                    if client.get_id() == actor_cell.get_id() {
                        debug!("Controller's own TCP client terminated, clearing reference");
                        state.client_ref = None;
                    }
                }
            }
            SupervisionEvent::ActorFailed(actor_cell, error) => {
                if let Some(client_ref) = &state.client_ref {
                    if client_ref.get_id() == actor_cell.get_id() {
                        warn!("Controller's own TCP client failed (will continue): {}", error);
                        state.client_ref = None;
                    } else {
                        warn!(
                            "Non‑critical worker agent {:?} failed (ignoring): {}",
                            actor_cell.get_name(),
                            error
                        );
                    }
                } else {
                    warn!(
                        "Worker agent {:?} failed (ignoring): {}",
                        actor_cell.get_name(),
                        error
                    );
                }
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
            HarnessControllerMessage::ShutdownTimeout => {
                if state.waiting_for_shutdown_confirmation {
                    warn!("Shutdown confirmation timeout, killing broker process directly");
                    if let Some(kill_tx) = state.broker_kill.take() {
                        let _ = kill_tx.send(());
                    }
                    if let Some(task) = state.broker_task.take() {
                        task.abort();
                    }
                    state.waiting_for_shutdown_confirmation = false;
                    myself.stop(Some("Shutdown timeout".to_string()));
                }
            }

            HarnessControllerMessage::CassiniEvent(event) => match event {
                ClientEvent::Registered { registration_id } => {
                    info!("Controller registered: {}", registration_id);
                    state.registration_id = Some(registration_id);
                    myself.cast(HarnessControllerMessage::StartTest).ok();
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Controller's own broker connection died: {}", reason);
                    state.client_ref = None;
                    if state.waiting_for_shutdown_confirmation {
                        if let Some(kill_tx) = state.broker_kill.take() {
                            let _ = kill_tx.send(());
                        }
                        if let Some(task) = state.broker_task.take() {
                            task.abort();
                        }
                        myself.stop(Some("Broker connection lost".to_string()));
                        return Ok(());
                    }
                }
                ClientEvent::ControlResponse { result, .. } => match result {
                    Ok(ControlResult::ShutdownInitiated) => {
                        info!("Broker shutdown initiated successfully");
                        if state.waiting_for_shutdown_confirmation {
                            state.waiting_for_shutdown_confirmation = false;
                            if let Some(handle) = state.shutdown_timeout.take() {
                                handle.abort();
                            }
                            if let Some(client_ref) = &state.client_ref {
                                let _ = client_ref.send_message(TcpClientMessage::Disconnect {
                                    trace_ctx: None,
                                });
                                state.client_ref = None;
                            }
                            // Wait for broker process to exit
                            if let Some(task) = state.broker_task.take() {
                                let myself = myself.clone();
                                tokio::spawn(async move {
                                    info!("Waiting for broker process to exit...");
                                    if let Err(e) = task.await {
                                        error!("Broker task failed: {}", e);
                                    }
                                    info!("Broker process exited, controller may now stop.");
                                    let _ = myself.send_message(HarnessControllerMessage::BrokerShutdownComplete);
                                });
                            } else {
                                // Already gone? signal completion directly
                                let _ = myself.send_message(HarnessControllerMessage::BrokerShutdownComplete);
                            }
                        }
                    }
                    Ok(_) => {
                        debug!("Received other control response: {:?}", result);
                    }
                    Err(e) => {
                        error!("Failed to initiate broker shutdown: {:?}", e);
                        if state.waiting_for_shutdown_confirmation {
                            state.waiting_for_shutdown_confirmation = false;
                            if let Some(kill_tx) = state.broker_kill.take() {
                                let _ = kill_tx.send(());
                            }
                            if let Some(task) = state.broker_task.take() {
                                task.abort();
                            }
                            myself.stop(Some("Shutdown failed".to_string()));
                        }
                    }
                },
                _ => (),
            },

            HarnessControllerMessage::InitiateShutdown { auth_token, .. } => {
                info!("Initiating broker graceful shutdown...");
                state.shutdown_initiated = true;
                Self::send_shutdown_command(myself.clone(), state, auth_token).await;
                return Ok(());
            }

            HarnessControllerMessage::BrokerShutdownComplete => {
                state.shutdown_initiated = true;
                info!("Broker shutdown complete, stopping controller.");
                myself.stop(Some("test complete".to_string()));
            }

            HarnessControllerMessage::TestEvent(event) => match event {
                TestEvent::CommandReceived { reply_to, command } => match command {
                    ControllerCommand::Hello { role } => match role {
                        AgentRole::Producer => {
                            debug!("Received hello from producer");
                            if let Some(test) = state.test_plan.tests.get(state.current_test_index) {
                                state.test_state.as_mut().map(|s| s.producer = Some(reply_to.clone()));
                                let reply = ControllerCommand::ProducerConfig {
                                    producer: test.producer.clone(),
                                };
                                reply_to.cast(ClientSessionMessage::Send(reply)).ok();
                            } else {
                                error!("No test found for current index!");
                                return Err(ActorProcessingErr::from("No test configured.".to_string()));
                            }
                        }
                        AgentRole::Sink => {
                            debug!("Received hello from sink");
                            if let Some(_test) = state.test_plan.tests.get(state.current_test_index) {
                                if let Some(test_state) = state.test_state.as_mut() {
                                    test_state.sink = Some(reply_to.clone());
                                }
                                HarnessController::issue_sink_configuration(state)?;
                            } else {
                                error!("No test found for current index!");
                                return Err(ActorProcessingErr::from("No test configured.".to_string()));
                            }
                        }
                    },
                    ControllerCommand::SinkReady => {
                        debug!("Sink is ready to receive messages");
                        if let Some(test_state) = state.test_state.as_mut() {
                            test_state.sink_ready = true;
                            // If the producer hasn't been started yet, start it now
                            if test_state.producer.is_none() {
                                // Spawn producer process (same as existing code)
                                state.producer = Some(HarnessController::start_client_process(state.producer_path.clone()).await);
                                // The producer will connect and send Hello; we'll handle that separately.
                            }
                        }
                    },
                    ControllerCommand::TestComplete { role, .. } => {
                        if let Some(test_state) = state.test_state.as_mut() {
                            match role {
                                AgentRole::Producer => {
                                    debug!("Producer agent completed task");
                                    test_state.producer_complete = true;
                                    if let Some(sink_session) = test_state.sink.as_ref() {
                                        sink_session.send_message(ClientSessionMessage::Send(
                                            ControllerCommand::ProducerFinished,
                                        ))?;
                                    } else {
                                        error!("Producer completed but no sink session exists – cannot proceed");
                                        myself.stop(Some("Producer completed but sink missing".to_string()));
                                        return Ok(());
                                    }
                                }
                                AgentRole::Sink => test_state.sink_complete = true,
                            }
                            if test_state.producer_complete && test_state.sink_complete {
                                debug!("Test {} completed!", state.current_test_index);
                                info!("Stopping all client sessions after test completion");
                                for (client_id, client_actor) in state.clients.drain() {
                                    if let Err(e) = client_actor.send_message(ClientSessionMessage::Stop) {
                                        warn!("Failed to send Stop to client {}: {}", client_id, e);
                                    } else {
                                        debug!("Stop sent to client {}", client_id);
                                    }
                                }
                                myself.cast(HarnessControllerMessage::AdvanceTest).ok();
                            } else {
                                debug!("Waiting for other agent to complete");
                            }
                        } else {
                            error!("No test state found");
                            myself.stop(Some("No test state found".to_string()))
                        }
                        info!("TestComplete handled, not stopping yet");
                    }
                    ControllerCommand::TestError { error } => {
                        error!("Test failed! {error}");
                        myself.stop(Some(error));
                    }
                    _ => warn!("Unexpected command! {command:?}"),
                },
                TestEvent::TransportError { client_id, reason } => {
                    if let Some(test_state) = &state.test_state {
                        if test_state.producer_complete || test_state.sink_complete {
                            info!(
                                "Transport error after one agent completed ({}): {} – ignoring",
                                client_id, reason
                            );
                            return Ok(());
                        }
                    }
                    let test_complete = state.current_test_index >= state.test_plan.tests.len();
                    if test_complete {
                        info!(
                            "Client {} disconnected after test completed (shutdown): {}",
                            client_id, reason
                        );
                        if let Some(client_actor) = state.clients.remove(&client_id) {
                            let _ = client_actor.send_message(ClientSessionMessage::Stop);
                        }
                        return Ok(());
                    }
                    error!("Transport error before test completion: {} for client {}", reason, client_id);
                    myself.stop(Some(reason));
                }
            },

            HarnessControllerMessage::StartTest => {
                if !state.started {
                    state.started = true;
                    state.sink = Some(HarnessController::start_client_process(state.sink_path.clone()).await);
                    // tokio::time::sleep(Duration::from_millis(500)).await;
                    // state.producer = Some(HarnessController::start_client_process(state.producer_path.clone()).await);
                    state.test_state = Some(TestState {
                        producer: None,
                        sink: None,
                        producer_complete: false,
                        sink_complete: false,
                        sink_ready: false,
                    });
                } else {
                    error!("Received start directive during active test. Stopping.");
                    myself.stop(Some("Duplicate start directive".to_string()));
                }
            }

            HarnessControllerMessage::AdvanceTest => {
                state.current_test_index += 1;
                match state.test_plan.tests.get(state.current_test_index) {
                    Some(_test) => {
                        debug!("Advancing to next test {}", state.current_test_index);
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
                        info!("All tests completed, initiating broker graceful shutdown");
                        let last_index = state.current_test_index.saturating_sub(1);
                        if let Some(last_test) = state.test_plan.tests.get(last_index) {
                            let expected = match &last_test.producer.pattern {
                                MessagePattern::Drip { idle_time_seconds } => {
                                    (last_test.producer.duration as f64 * (1.0 / *idle_time_seconds as f64))
                                        .ceil() as usize
                                }
                                MessagePattern::Burst {
                                    burst_size,
                                    idle_time_seconds,
                                } => {
                                    (last_test.producer.duration as f64 * (1.0 / *idle_time_seconds as f64))
                                        .ceil() as usize
                                        * burst_size
                                }
                            };
                            state.expected_message_count = Some(expected);
                            info!("Expected messages for last test: {}", expected);
                        }
                        let shutdown_token = std::env::var("BROKER_SHUTDOWN_TOKEN").ok();
                        info!(
                            "Sending broker shutdown command (token present: {})",
                            shutdown_token.is_some()
                        );
                        myself.cast(HarnessControllerMessage::InitiateShutdown {
                            auth_token: shutdown_token,
                            timeout: None,
                        })?;
                        return Ok(());
                    }
                }
            }

            HarnessControllerMessage::ClientConnected { client_id, client, .. } => {
                if state.clients.insert(client_id.clone(), client).is_some() {
                    warn!("Client {} already existed, replacing", client_id);
                } else {
                    debug!("Client {} added to map", client_id);
                }
                debug!("Client connected: {}", client_id);
            }
        }

        Ok(())
    }
}

// Add a helper for starting client processes (sink/producer)
impl HarnessController {
    async fn start_client_process(binary_path: String) -> JoinHandle<()> {
        tokio::spawn(async move {
            tracing::info!("Spawning binary at {binary_path}...");
            let mut child = Command::new(binary_path)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("failed to spawn client");
            let _status = child.wait().await.expect("client crashed");
        })
    }
}

// ================== ClientSession actor ==================

struct ClientSession;

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
    Stop,
}

struct ClientSessionState {
    writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    task_handle: Option<JoinHandle<()>>,
    client_id: String,
    output_port: Arc<OutputPort<TestEvent>>,
    shutdown_token: CancellationToken,
}

struct ClientSessionArguments {
    writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    client_id: String,
    output_port: OutputPort<TestEvent>,
}

impl ClientSession {
    async fn write(
        message: ControllerCommand,
        writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    ) -> Result<(), Error> {
        match rkyv::to_bytes::<Error>(&message) {
            Ok(bytes) => {
                let len_u32: u32 = bytes.len().try_into().map_err(|_| {
                    Error::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "frame too large for u32 length prefix",
                    ))
                })?;

                let mut buffer = Vec::with_capacity(4 + bytes.len());
                buffer.extend_from_slice(&len_u32.to_be_bytes());
                buffer.extend_from_slice(&bytes);

                tokio::spawn(async move {
                    let mut writer = writer.lock().await;
                    if let Err(e) = writer.write_all(&buffer).await {
                        warn!("Failed to send message to client {e}");
                        return Err(Error::new(e));
                    }
                    if let Err(e) = writer.flush().await {
                        return Err(Error::new(e));
                    }
                    Ok(())
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
            shutdown_token: CancellationToken::new(),
        })
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("ClientSession {} shutting down gracefully", state.client_id);

        state.shutdown_token.cancel();

        if let Some(handle) = state.task_handle.take() {
            let _ = tokio::time::timeout(Duration::from_millis(100), handle).await;
        }

        let mut writer = state.writer.lock().await;
        let _ = writer.flush().await;
        let _ = writer.shutdown().await;

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
        let shutdown_token = state.shutdown_token.clone();

        debug!("client listening...");

        let handle = tokio::spawn(async move {
            let mut buf_reader = tokio::io::BufReader::new(reader);
            loop {
                tokio::select! {
                    result = buf_reader.read_u32() => {
                        match result {
                            Ok(incoming_msg_length) => {
                                if incoming_msg_length > 0 {
                                    let mut buffer = vec![0u8; incoming_msg_length as usize];
                                    if let Ok(_) = buf_reader.read_exact(&mut buffer).await {
                                        match rkyv::access::<harness_common::ArchivedControllerCommand, rkyv::rancor::Error>(&buffer[..]) {
                                            Ok(archived) => {
                                                if let Ok(command) = rkyv::deserialize::<ControllerCommand, rkyv::rancor::Error>(archived) {
                                                    let message = HarnessControllerMessage::TestEvent(
                                                        TestEvent::CommandReceived {
                                                            reply_to: myself.clone(),
                                                            command,
                                                        },
                                                    );
                                                    if let Some(service) = cloned_self.try_get_supervisor() {
                                                        let _ = service.send_message(message);
                                                    }
                                                }
                                            }
                                            Err(e) => warn!("Failed to parse message: {e}"),
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                output_port.send(TestEvent::TransportError {
                                    client_id: client_id.clone(),
                                    reason: format!("read error: {}", e),
                                });
                                break;
                            }
                        }
                    }
                    _ = shutdown_token.cancelled() => {
                        debug!("Read task received shutdown signal, exiting cleanly");
                        break;
                    }
                }
            }
            let _ = cloned_self.send_message(ClientSessionMessage::Stop);
        });

        state.task_handle = Some(handle);
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ClientSessionMessage::Send(message) => {
                ClientSession::write(message, Arc::clone(&state.writer))
                    .await
                    .expect("expected to write message to client.");
            }
            ClientSessionMessage::Stop => {
                debug!("ClientSession {} stopping due to connection close", state.client_id);
                myself.stop(Some("CONNECTION_CLOSED".to_string()));
            }
        }
        Ok(())
    }
}
