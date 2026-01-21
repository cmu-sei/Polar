use crate::{Phase, TestPlan};

use ractor::{
    async_trait,
    concurrency::{Instant, JoinHandle},
    Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent,
};
use std::collections::HashSet;
use std::sync::Arc;
use std::{process::Stdio, time::Duration};

use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::{ClientEvent, ClientMessage, ControlError, ControlResult, SessionDetails};
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
    pub current_phase_index: usize,
    started: bool,
    client_ref: Option<ActorRef<TcpClientMessage>>,
    bind_addr: String,
    server_config: ServerConfig,
    registration_id: Option<String>,
    pub current_phase: Option<Phase>,
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
            current_phase_index: 0,
            current_phase: None,
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
                    ClientEvent::TransportError { reason } => {
                        error!("Client transport error: {}", reason);
                        // if transport fails, consider failing the test
                        return Err(ActorProcessingErr::from("Transport error"));
                    }
                    _ => (),
                }
            }

            HarnessControllerMessage::StartTest => {
                if state.started {
                    // idempotent
                } else {
                    state.started = true;
                    // ensure index starts at 0
                    state.current_phase_index = 0;
                    // start processing
                    // schedule async evaluation of next step
                    let _ = myself.cast(HarnessControllerMessage::AdvanceTest);
                }
            }

            HarnessControllerMessage::AdvanceTest => {
                // TODO: process next step
                todo!("implement phase orchestration");
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
