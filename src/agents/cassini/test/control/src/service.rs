use crate::{
    Action, ActiveGate, Observation, ObservedBrokerState, SessionExpectation, Step, TestPlan,
    TopicExpectation,
};
use cassini_types::{ControlOp, SessionMap};

use ractor::{
    async_trait,
    concurrency::{Instant, JoinHandle},
    Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent,
};
use std::collections::HashSet;
use std::sync::Arc;
use std::{process::Stdio, time::Duration};

use cassini_client::{
    ClientEvent, TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage,
};
use cassini_types::{ClientMessage, ControlError, ControlResult, SessionDetails};
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

#[derive(Debug, Clone)]
pub enum HarnessControllerMessage {
    Initialize,
    ClientEvent(ClientEvent),
    StartTest,
    AdvanceTest,
    StepCompleted,
    ShutdownAck,
    Error { reason: String },
}

pub struct HarnessController;

impl HarnessController {
    /// Helper to run an instance of the broker in another thread.
    pub async fn spawn_broker(broker_path: String) -> AbortHandle {
        return tokio::spawn(async move {
            tracing::info!("Spawning broker...");
            let mut child = Command::new(broker_path)
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

    /// Process the next step (non-blocking). Called whenever we need to advance.
    /// It will:
    ///  - if out of steps: emit ShutdownAck
    ///  - if Step::Do: run action (may spawn process or send control request)
    ///  - if Step::Wait: set active gate and attempt immediate evaluation
    async fn process_next_step(
        &self,
        myself: ActorRef<HarnessControllerMessage>,
        state: &mut HarnessControllerState,
    ) -> Result<(), ActorProcessingErr> {
        if state.current_step_index >= state.test_plan.steps.len() {
            // plan complete
            myself.cast(HarnessControllerMessage::ShutdownAck).ok();
            return Ok(());
        }

        match &state.test_plan.steps[state.current_step_index] {
            Step::Do(action) => {
                // perform action; if action is immediate, consider it completed (or wait for explicit signal)
                self.handle_do_action(action.clone(), myself.clone(), state)
                    .await?;
                // By default, treat Do as immediately completed and advance. If some Do requires waiting,
                // you'd install a gate or expect some observation to validate it.
                self.advance_step(myself, state)?;
            }

            Step::Wait(gate) => {
                // Install an active gate with deadline and evaluate immediately
                let deadline = Instant::now() + Duration::from_secs(gate.timeout_seconds as u64);
                state.active_gate = Some(ActiveGate {
                    deadline,
                    expectations: gate.expect.clone(),
                });

                // Immediately evaluate (in case observed state already satisfies)
                if self.check_current_gate(state) {
                    // satisfied right away
                    self.advance_step(myself, state)?;
                } else {
                    // else we'll wait for observations to come in and re-evaluate
                    tracing::info!("Waiting on gate: {:?}", gate.description);
                }
            }
        }

        Ok(())
    }

    /// Advance to next step (increment index and schedule processing of next step).
    fn advance_step(
        &self,
        myself: ActorRef<HarnessControllerMessage>,
        state: &mut HarnessControllerState,
    ) -> Result<(), ActorProcessingErr> {
        state.current_step_index += 1;
        state.active_gate = None;
        // enqueue processing of the next step
        myself
            .cast(HarnessControllerMessage::AdvanceTest)
            .map_err(ActorProcessingErr::from)
    }

    /// Evaluate the current active gate (if any) against the observed state.
    /// Returns true if satisfied.
    fn check_current_gate(&self, state: &HarnessControllerState) -> bool {
        let idx = state.current_step_index;
        let step = match state.test_plan.steps.get(idx) {
            Some(Step::Wait(g)) => g,
            _ => return false, // not a wait step
        };

        let expect = &step.expect;

        // Sessions
        if let Some(s) = &expect.sessions {
            match s {
                SessionExpectation::CountAtLeast(n) => {
                    if state.observed_state.sessions.len() < (*n as usize) {
                        return false;
                    }
                }
                SessionExpectation::ContainsIds(ids) => {
                    for id in ids {
                        if !state.observed_state.sessions.contains_key(id) {
                            return false;
                        }
                    }
                }
            }
        }

        // Topics
        // if let Some(t) = &expect.topics {
        //     match t {
        //         TopicExpectation::Exists(list) => {
        //             for topic in list {
        //                 if !state.observed_state.topics.contains(topic) {
        //                     return false;
        //                 }
        //             }
        //         }
        //         TopicExpectation::CountAtLeast(n) => {
        //             if state.observed_state.topics.len() < (*n as usize) {
        //                 return false;
        //             }
        //         }
        //     }
        // }

        // Subscriptions
        // if let Some(sub) = &expect.subscriptions {
        //     let count = state
        //         .observed_state
        //         .subscriptions
        //         .get(&sub.topic)
        //         .cloned()
        //         .unwrap_or(0);
        //     if count < sub.countAtLeast as usize {
        //         return false;
        //     }
        // }

        true
    }

    /// Handle Step::Do actions. This function should send control requests or spawn processes.
    async fn handle_do_action(
        &self,
        action: Action,
        _myself: ActorRef<HarnessControllerMessage>,
        state: &mut HarnessControllerState,
    ) -> Result<(), ActorProcessingErr> {
        match action {
            Action::StartProducer(spec) => {
                // Spawn the producer as earlier via tokio::spawn and set state.producer handle.
                // This is your existing code; keep process management in harness.
                let path = state.producer_path.clone();
                let client = spec.client_id.clone();
                // Example: spawn process with args derived from spec.producer
                let _handle = tokio::spawn(async move {
                    let mut child = Command::new(path)
                        .arg("--producer-config")
                        // write spec as temp file or pass via env, omitted here
                        .spawn()
                        .expect("failed to spawn producer");
                    let status = child.wait().await.expect("producer crashed");
                    tracing::warn!(?status, "Producer exited");
                })
                .abort_handle();
                state.producer = Some(_handle);
            }

            Action::Subscribe(s) => {
                // Send a subscribe control via the client actor
                if let Some(client_ref) = state.client_ref.as_ref() {
                    // The TcpClientMessage::Subscribe variant exists in your client; use send_message
                    client_ref
                        .send_message(TcpClientMessage::Subscribe(s.topic))
                        .map_err(ActorProcessingErr::from)?;
                } else {
                    return Err(ActorProcessingErr::from("No client_ref available"));
                }
            }

            Action::Unsubscribe(u) => {
                if let Some(client_ref) = state.client_ref.as_ref() {
                    client_ref
                        .send_message(TcpClientMessage::UnsubscribeRequest(u.topic))
                        .map_err(ActorProcessingErr::from)?;
                } else {
                    return Err(ActorProcessingErr::from("No client_ref available"));
                }
            }

            Action::Disconnect(d) => {
                if let Some(client_ref) = state.client_ref.as_ref() {
                    client_ref
                        .send_message(TcpClientMessage::Disconnect)
                        .map_err(ActorProcessingErr::from)?;
                } else {
                    return Err(ActorProcessingErr::from("No client_ref available"));
                }
            }
        }

        Ok(())
    }

    fn derive_observation(result: ControlResult) -> Option<Observation> {
        match result {
            ControlResult::SessionList(sessions) => Some(Observation::Sessions(sessions)),

            ControlResult::TopicList(topics) => {
                Some(Observation::Topics(topics.into_iter().collect()))
            }

            _ => None,
        }
    }
    /// Try to parse a raw message blob (the first item from queue_output) and, if it
    /// is a ControlResponse, convert it into an Observation that the controller can use.
    ///
    /// Returns Ok(Some(obs)) when a meaningful observation was produced,
    /// Ok(None) when the message isn't relevant, and Err(_) when deserialization failed.
    pub fn parse_control_response(buffer: &[u8]) -> Result<Option<Observation>, ControlError> {
        // Use rkyv to deserialize the wire bytes (ClientMessage)
        match rkyv::from_bytes::<ClientMessage, rancor::Error>(buffer) {
            Ok(client_msg) => {
                match client_msg {
                    ClientMessage::ControlResponse {
                        registration_id: _id,
                        result,
                    } => {
                        // Convert ControlResult -> Observation where reasonable
                        match result {
                            Ok(control_res) => {
                                // Map concrete control results to observations.
                                // Adjust these arms to match your ControlResult shape.
                                match control_res {
                                    // If you have a SessionInfo variant that contains SessionDetails type
                                    ControlResult::SessionInfo(session_details) => {
                                        return Ok(Some(Observation::Session(session_details)));
                                    }

                                    // If the control returned a list of sessions
                                    ControlResult::SessionList(list) => {
                                        return Ok(Some(Observation::Sessions(list)));
                                    }

                                    // If the control returned a list of topic names
                                    ControlResult::TopicList(vec_topics) => {
                                        let set: HashSet<String> = vec_topics.into_iter().collect();
                                        return Ok(Some(Observation::Topics(set)));
                                    }

                                    // Other kinds we don't treat as observations (Pong, BrokerStats, etc.)
                                    _ => return Ok(None),
                                }
                            }
                            Err(control_err) => {
                                // Control returned an error; log and optionally surface as observation
                                tracing::warn!("Control op returned error: {:?}", control_err);
                                // If you want to convert some errors into observations, do that here.
                                return Ok(None);
                            }
                        }
                    }

                    // Not a control response — ignore
                    _ => Ok(None),
                }
            }
            Err(e) => {
                // Deserialisation failed — return an error so the subscriber can log it
                tracing::error!("Failed to deserialize client message: {:?}", e);
                Err(ControlError::InternalError(format!(
                    "Failed to deserialize control response: {:?}",
                    e
                )))
            }
        }
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
            Some(HarnessControllerMessage::ClientEvent(event))
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
}

pub struct HarnessControllerState {
    pub current_step_index: usize,
    started: bool,
    client_ref: Option<ActorRef<TcpClientMessage>>,
    bind_addr: String,
    server_config: ServerConfig,
    registration_id: Option<String>,
    pub active_gate: Option<ActiveGate>,
    pub test_plan: TestPlan,
    pub observed_state: ObservedBrokerState,
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

        let plan = args.test_plan.clone();
        let cloned_self = myself.clone();
        // TODO: unfreeze when we're ready to add clients
        // let server_handle = tokio::spawn(async move {
        //     while let Ok((stream, peer_addr)) = server.accept().await {
        //         match acceptor.accept(stream).await {
        //             Ok(stream) => {
        //                 let client_id = uuid::Uuid::new_v4().to_string();

        //                 let (reader, writer) = split(stream);

        //                 let writer = tokio::io::BufWriter::new(writer);

        //                 debug!("New connection from {peer_addr}");

        //                 let listener_args = ClientSessionArguments {
        //                     writer: Arc::new(Mutex::new(writer)),
        //                     reader: Some(reader),
        //                     // client_id: client_id.clone(),
        //                 };

        //                 //start listener actor to handle connection
        //                 let (client, _) = Actor::spawn_linked(
        //                     Some(client_id),
        //                     ClientSession,
        //                     listener_args,
        //                     cloned_self.clone().into(),
        //                 )
        //                 .await
        //                 .expect("Failed to start listener for new connection");

        //                 // send test plan
        //                 client
        //                     .cast(ClientSessionMessage::Send(
        //                         HarnessControllerMessage::TestPlan { plan: plan.clone() },
        //                     ))
        //                     .ok();
        //             }
        //             Err(e) => {
        //                 //We probably got pinged or something, ignore but log the attempt.
        //                 tracing::warn!("TLS handshake failed from {}: {}", peer_addr, e);
        //             }
        //         }
        //     }
        // })
        // .abort_handle();

        //set up state object
        let state = HarnessControllerState {
            client_ref: None,
            started: false,
            current_step_index: 0,
            observed_state: ObservedBrokerState::default(),
            active_gate: None,
            registration_id: None,
            bind_addr: args.bind_addr,
            server_config,
            test_plan: args.test_plan,
            server_handle: None, // TODO: Add back the server handle
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

        tracing::info!("Starting cassini server...");
        let broker = HarnessController::spawn_broker(state.broker_path.clone()).await;
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
            HarnessControllerMessage::ClientEvent(event) => {
                match event {
                    ClientEvent::Registered { registration_id } => {
                        info!("Controller registered: {}", registration_id);

                        state.registration_id = Some(registration_id);

                        // Kick off the test now that we're registered
                        // This will queue StartTest and then the actor will process the first step.
                        myself.cast(HarnessControllerMessage::StartTest).ok();
                    }

                    ClientEvent::MessagePublished {
                        topic: _topic,
                        payload,
                    } => {
                        // parse and convert to Observation
                        match HarnessController::parse_control_response(&payload) {
                            Ok(Some(obs)) => {
                                state.observed_state.apply(obs);
                                // After applying an observation, re-evaluate any active gate
                                if let Some(active) = &state.active_gate {
                                    // check deadline
                                    if Instant::now() > active.deadline {
                                        return Err(ActorProcessingErr::from("Gate timed out"));
                                    }
                                }

                                // If the active gate is satisfied, advance
                                if self.check_current_gate(state) {
                                    self.advance_step(myself, state)?;
                                }
                            }
                            Ok(None) => {
                                // ignore irrelevant messages
                            }
                            Err(e) => {
                                return Err(ActorProcessingErr::from(
                                    "Failed parsing control response.".to_string(),
                                ));
                            }
                        }
                    }

                    ClientEvent::TransportError { reason } => {
                        error!("Client transport error: {}", reason);
                        // if transport fails, consider failing the test
                        return Err(ActorProcessingErr::from("Transport error"));
                    }
                }
            }

            HarnessControllerMessage::StartTest => {
                if state.started {
                    // idempotent
                } else {
                    state.started = true;
                    // ensure index starts at 0
                    state.current_step_index = 0;
                    // start processing
                    // schedule async evaluation of next step
                    let _ = myself.cast(HarnessControllerMessage::AdvanceTest);
                }
            }

            HarnessControllerMessage::AdvanceTest => {
                // process next step
                self.process_next_step(myself.clone(), state).await?;
            }

            HarnessControllerMessage::ShutdownAck => {
                // finish test
                myself.stop(Some("TEST_COMPLETE".to_string()));
            }

            _ => (),
        }

        Ok(())
    }
}

// struct ClientSession;

// enum ClientSessionMessage {
//     Send(HarnessControllerMessage),
// }

// struct ClientSessionState {
//     writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
//     reader: Option<ReadHalf<TlsStream<TcpStream>>>,
//     task_handle: Option<JoinHandle<()>>,
//     // output_port: Arc<OutputPort<SinkCommand>>,
// }

// struct ClientSessionArguments {
//     writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
//     reader: Option<ReadHalf<TlsStream<TcpStream>>>,
//     // output_port: Arc<OutputPort<SinkCommand>>,
// }

// impl ClientSession {
//     /// Helper fn to write messages to client
//     ///
//     /// writing binary data over the wire requires us to be specific about what we expect on the other side. So we note the length of the message
//     /// we intend to send and write the amount to the *front* of the buffer to be read first, then write the actual message data to the buffer to be deserialized.
//     ///
//     async fn write(
//         message: HarnessControllerMessage,
//         writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
//     ) -> Result<(), Error> {
//         match rkyv::to_bytes::<Error>(&message) {
//             Ok(bytes) => {
//                 //create new buffer
//                 let mut buffer = Vec::new();

//                 //get message length as header
//                 let len = bytes.len().to_be_bytes();
//                 buffer.extend_from_slice(&len);

//                 //add message to buffer
//                 buffer.extend_from_slice(&bytes);

//                 tokio::spawn(async move {
//                     let mut writer = writer.lock().await;
//                     if let Err(e) = writer.write_all(&buffer).await {
//                         warn!("Failed to send message to client {e}");
//                         return Err(rancor::Error::new(e));
//                     }
//                     Ok(if let Err(e) = writer.flush().await {
//                         return Err(rancor::Error::new(e));
//                     })
//                 })
//                 .await
//                 .expect("Expected write thread to finish")
//             }
//             Err(e) => Err(e),
//         }
//     }
// }

// #[async_trait]
// impl Actor for ClientSession {
//     type Msg = ClientSessionMessage;
//     type State = ClientSessionState;
//     type Arguments = ClientSessionArguments;

//     async fn pre_start(
//         &self,
//         _: ActorRef<Self::Msg>,
//         args: ClientSessionArguments,
//     ) -> Result<Self::State, ActorProcessingErr> {
//         Ok(ClientSessionState {
//             writer: args.writer,
//             reader: args.reader,
//             task_handle: None,
//             // output_port: args.output_port,
//         })
//     }

//     async fn post_stop(
//         &self,
//         _myself: ActorRef<Self::Msg>,
//         state: &mut Self::State,
//     ) -> Result<(), ActorProcessingErr> {
//         // stop listening
//         //
//         state.task_handle.as_ref().map(|handle| {
//             handle.abort();
//         });
//         Ok(())
//     }

//     async fn post_start(
//         &self,
//         myself: ActorRef<Self::Msg>,
//         state: &mut Self::State,
//     ) -> Result<(), ActorProcessingErr> {
//         let reader = state.reader.take().expect("Reader already taken!");
//         let cloned_self = myself.clone();

//         //start listening
//         debug!("client listening...");
//         let handle = tokio::spawn(async move {
//             let mut buf_reader = tokio::io::BufReader::new(reader);
//             // parse incoming message length, this tells us what size of a message to expect.
//             while let Ok(incoming_msg_length) = buf_reader.read_u64().await {
//                 if incoming_msg_length > 0 {
//                     // create a buffer of exact size, and read data in.
//                     let mut buffer = vec![0u8; incoming_msg_length as usize];
//                     if let Ok(_) = buf_reader.read_exact(&mut buffer).await {
//                         // use unsafe API for maximum performance
//                         match rkyv::access::<ArchivedHarnessControllerMessage, Error>(&buffer[..]) {
//                             Ok(archived) => {
//                                 // deserialize back to the original type
//                                 if let Ok(deserialized) =
//                                     deserialize::<HarnessControllerMessage, Error>(archived)
//                                 {
//                                     cloned_self.try_get_supervisor().map(|service| {
//                                         service
//                                             .send_message(deserialized)
//                                             .expect("Expected to forward command upwards");
//                                     });
//                                 }
//                             }
//                             Err(e) => {
//                                 warn!("Failed to parse message: {e}");
//                             }
//                         }
//                     }
//                 }
//             }
//         });

//         // add join handle to state, we'll need to cancel it if a client disconnects
//         state.task_handle = Some(handle);

//         // // send client service its client_id
//         // let client_id = myself
//         //     .get_name()
//         //     .expect("Expected client session to be named");

//         // debug!("issuing client id.");
//         // ClientSession::write(
//         //     HarnessControllerMessage::ClientRegistered(client_id),
//         //     state.writer.clone(),
//         // )
//         // .await
//         // .ok();

//         Ok(())
//     }

//     async fn handle(
//         &self,
//         _: ActorRef<Self::Msg>,
//         message: Self::Msg,
//         state: &mut Self::State,
//     ) -> Result<(), ActorProcessingErr> {
//         match message {
//             ClientSessionMessage::Send(message) => {
//                 ClientSession::write(message, Arc::clone(&state.writer))
//                     .await
//                     .expect("expected to write message to client.");
//             }
//         }
//         Ok(())
//     }
// }
