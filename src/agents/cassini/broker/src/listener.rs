use crate::UNEXPECTED_MESSAGE_STR;
use crate::{
    BrokerMessage, BROKER_NAME, BROKER_NOT_FOUND_TXT, LISTENER_MGR_NOT_FOUND_TXT,
    PUBLISH_REQ_FAILED_TXT, REGISTRATION_REQ_FAILED_TXT, SESSION_MISSING_REASON_STR,
    SESSION_NOT_FOUND_TXT, SUBSCRIBE_REQUEST_FAILED_TXT, TIMEOUT_REASON,
};
use async_trait::async_trait;
use cassini_types::{ArchivedClientMessage, ClientMessage};
use opentelemetry::Context;
use ractor::{registry::where_is, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use rkyv::{
    deserialize,
    rancor::{self, Error, Source},
};
use rustls::{
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
    server::WebPkiClientVerifier,
    RootCertStore, ServerConfig,
};
use std::sync::Arc;
use tokio::{
    io::{split, AsyncReadExt, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task::JoinHandle,
};
use tokio_rustls::{server::TlsStream, TlsAcceptor};
use tracing::{debug, debug_span, error, info, info_span, instrument, trace, trace_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;
// ============================== Listener Manager ============================== //
/// The actual listener/server process. When clients connect to the server, their stream is split and
/// given to a worker processes to use to interact with and handle that connection with the client.

pub struct ListenerManager;

pub struct ListenerManagerState {
    bind_addr: String,
    server_config: Arc<ServerConfig>,
}

pub struct ListenerManagerArgs {
    pub bind_addr: String,
    pub server_cert_file: String,
    pub private_key_file: String,
    pub ca_cert_file: String,
}

#[async_trait]
impl Actor for ListenerManager {
    type Msg = BrokerMessage;
    type State = ListenerManagerState;
    type Arguments = ListenerManagerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ListenerManagerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        let span = debug_span!("listener_manager.init");
        let _g = span.enter();

        debug!("ListenerManager: Starting {myself:?}");

        // install default crypto provider
        let provider = rustls::crypto::aws_lc_rs::default_provider().install_default();
        if let Err(_) = provider {
            debug!("Crypto provider configured");
        }

        tracing::debug!("ListenerManager: Gathering certificates for mTLS");
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

        debug!("ListenerManager: Building configuration for mTLS ");

        let server_config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, private_key)
            .expect("bad certificate/key");

        //set up state object
        let state = ListenerManagerState {
            bind_addr: args.bind_addr,
            server_config: Arc::new(server_config),
        };
        debug!("ListenerManager: Agent starting");
        Ok(state)
    }

    /// Once a the manager is running as a process, start the server
    /// and listen for incoming connections
    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let span = info_span!("listener_manager.serve");
        let _g = span.enter();

        let bind_addr = state.bind_addr.clone();
        let acceptor = TlsAcceptor::from(Arc::clone(&state.server_config));

        drop(_g);
        let server = TcpListener::bind(bind_addr.clone())
            .await
            .expect("could not start tcp listener");

        info!("Server running on {bind_addr}");

        // Handle incoming connections on a seperate thread so the listener manager doesn't get blocked
        let _ = tokio::spawn(async move {
            while let Ok((stream, peer_addr)) = server.accept().await {
                match acceptor.accept(stream).await {
                    Ok(stream) => {
                        // Generate a unique client ID
                        let client_id = uuid::Uuid::new_v4().to_string();

                        // Create and start a new Listener actor for this connection

                        let (reader, writer) = split(stream);

                        let writer = tokio::io::BufWriter::new(writer);

                        let listener_args = ListenerArguments {
                            writer: Arc::new(Mutex::new(writer)),
                            reader: Some(reader),
                            client_id: client_id.clone(),
                            registration_id: None,
                        };

                        //start listener actor to handle connection
                        let _ = Actor::spawn_linked(
                            Some(client_id.clone()),
                            Listener,
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
                debug!(
                    "Worker agent: {0:?}:{1:?} started",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorTerminated(actor_cell, boxed_state, reason) => {
                let client_id = actor_cell
                    .get_name()
                    .expect("Expected client listener to have been named");

                debug!(
                    "Client listener: {0}:{1:?} stopped. {reason:?}",
                    client_id,
                    actor_cell.get_id()
                );

                if let Some(reason) = reason {
                    if reason == TIMEOUT_REASON {
                        // tell the session we're timing out
                        boxed_state.map(|mut boxed| {
                            let state = boxed
                                .take::<ListenerState>()
                                .expect("Expected failed listener to have had a state");

                            if let Some(registration_id) = state.registration_id {
                                if let Some(session) = where_is(registration_id.clone()) {
                                    warn!("{client_id:?} disconnected unexpectedly!");

                                    if let Err(e) =
                                        session.send_message(BrokerMessage::TimeoutMessage {
                                            client_id,
                                            registration_id: registration_id.clone(),
                                            error: Some(TIMEOUT_REASON.to_string()),
                                        })
                                    {
                                        warn!("{SESSION_NOT_FOUND_TXT}: {registration_id}: {e}");
                                    }
                                }
                            }
                        });
                    }
                }
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
        match message {
            BrokerMessage::RegistrationResponse {
                client_id,
                result,
                trace_ctx,
            } => {
                let span = trace_span!(
                    "listener_manager.handle_registration_request",
                    %client_id
                );
                trace_ctx.clone().map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("Listener manager has received a registration response.");
                //forwward registration_id back to listener to signal success
                info!("Forwarding registration ack to listener: {client_id}");

                where_is(client_id.clone())
                    .unwrap()
                    .send_message(BrokerMessage::RegistrationResponse {
                        client_id,
                        result,
                        trace_ctx: trace_ctx.clone(),
                    })
                    .expect("Failed to forward message to client: {client_id}");
            }
            BrokerMessage::DisconnectRequest {
                client_id,
                trace_ctx,
                ..
            } => {
                let span = trace_span!("listener_manager.handle_disconnect_request");
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("listener manager received disconnect request");
                match where_is(client_id.clone()) {
                    Some(listener) => listener.stop(Some("{DISCONNECTED_REASON}".to_string())),
                    None => warn!("Couldn't find listener {client_id}"),
                }
            }
            BrokerMessage::TimeoutMessage { client_id, .. } => match where_is(client_id.clone()) {
                Some(listener) => listener.stop(Some(TIMEOUT_REASON.to_string())),
                None => warn!("Couldn't find listener {client_id}"),
            },
            _ => (),
        }
        Ok(())
    }
}

// ============================== Listener actor ============================== //
/// The Listener is the actor responsible for maintaining services' connection to the broker
/// and interpreting client messages.
/// All comms are forwarded to the broker via the sessionAgent after being validated.
struct Listener;

struct ListenerState {
    writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    client_id: String,
    registration_id: Option<String>,
    task_handle: Option<JoinHandle<()>>,
}

struct ListenerArguments {
    writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    client_id: String,
    registration_id: Option<String>,
}

impl Listener {
    /// Helper fn to write messages to client
    ///
    /// writing binary data over the wire requires us to be specific about what we expect on the other side. So we note the length of the message
    /// we intend to send and write the amount to the *front* of the buffer to be read first, then write the actual message data to the buffer to be deserialized.
    ///
    #[instrument(level = "trace", fields(client_id=client_id))]
    async fn write(
        client_id: String,
        message: ClientMessage,
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

                trace!("writing {} byte(s) to client.", buffer.len());

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
impl Actor for Listener {
    type Msg = BrokerMessage;
    type State = ListenerState;
    type Arguments = ListenerArguments;

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        args: ListenerArguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(ListenerState {
            writer: args.writer,
            reader: args.reader,
            client_id: args.client_id.clone(),
            registration_id: args.registration_id,
            task_handle: None,
        })
    }

    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // stop listening

        state.task_handle.as_ref().map(|handle| {
            handle.abort();
        });

        info!("Client {} disconnected.", myself.get_name().unwrap());
        Ok(())
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let span = info_span!("listener.init");
        let _g = span.enter();

        info!(
            "Listener: Listener started for client_id: {}",
            state.client_id.clone()
        );

        let id = state.client_id.clone();

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
                        match rkyv::access::<ArchivedClientMessage, Error>(&buffer[..]) {
                            Ok(archived) => {
                                // deserialize back to the original type
                                if let Ok(deserialized) =
                                    deserialize::<ClientMessage, Error>(archived)
                                {
                                    //convert datatype to broker_meessage, fields will be populated during message handling
                                    let converted_msg = BrokerMessage::from_client_message(
                                        deserialized,
                                        id.clone(),
                                    );

                                    myself
                                        .send_message(converted_msg)
                                        .expect("Could not forward message to handler");
                                }
                            }
                            Err(e) => {
                                warn!("Failed to parse message: {e}");
                            }
                        }
                    }
                }
            }

            // TODO: on disconnect, die with honor or allow other events to capture it?
            // myself.stop(Some(TIMEOUT_REASON.to_string()));
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
        match message {
            BrokerMessage::RegistrationRequest {
                registration_id,
                client_id,
                ..
            } => {
                // start the beginning of registration logging flow, this span context should be the root
                // of the logging tree
                // since all interactions from the client start here, here's where we begin logging it
                let span = trace_span!(
                    "listener.handle_registration_request",
                    client_id = client_id
                );
                // explicitly break context so we get a new trace for each new request
                // We'll do this for every message or execution flow that happens across persistent actors
                span.set_parent(Context::new()).ok();
                let _g = span.enter();
                //send message to session that it has a new listener for it to get messages from
                //forward to broker to perform any auth and potentially resume session
                trace!(
                    "listener {client_id} received registration request.",
                    client_id = myself
                        .get_name()
                        .expect("Expected client to have been named.")
                );

                match where_is(BROKER_NAME.to_string()) {
                    Some(broker) => {
                        if let Err(e) = broker.send_message(BrokerMessage::RegistrationRequest {
                            registration_id,
                            client_id: client_id.clone(),
                            trace_ctx: Some(span.context()),
                        }) {
                            let err_msg = format!(
                                "{REGISTRATION_REQ_FAILED_TXT}: {BROKER_NOT_FOUND_TXT}: {e}"
                            );
                            error!("{err_msg}");
                            let msg = ClientMessage::RegistrationResponse {
                                result: Err(err_msg.clone()),
                            };

                            drop(_g);
                            Listener::write(client_id, msg, Arc::clone(&state.writer))
                                .await
                                .map_err(|e| error!("Failed to write message {e}"))
                                .ok();

                            myself.stop(Some(err_msg));
                        }
                    }
                    // Clearly, if there's no root broker actor the whole thing collapses, but the client should get something back, no?
                    // Mayube we can test if this ever has the chance to occur, otherwise we should probably just inspect the result of where_is()
                    None => {
                        let err_msg =
                            format!("{REGISTRATION_REQ_FAILED_TXT}: {BROKER_NOT_FOUND_TXT}");
                        error!("{err_msg}");
                        let msg = ClientMessage::RegistrationResponse {
                            result: Err(err_msg.clone()),
                        };

                        Listener::write(client_id, msg, Arc::clone(&state.writer))
                            .await
                            .map_err(|e| {
                                error!("Failed to write message {e}");
                                if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                    client_id: myself.get_name().unwrap(),
                                    registration_id: registration_id.unwrap(),
                                    error: Some(e.to_string()),
                                }) {
                                    myself.stop(Some(e.to_string()));
                                }
                            })
                            .ok();

                        myself.stop(Some(err_msg));
                    }
                }
            }
            BrokerMessage::RegistrationResponse {
                client_id,
                result,
                trace_ctx,
            } => {
                let span = trace_span!(
                    "listener.handle_registration_response",
                    client_id = client_id
                );
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("Listener actor received registration response");

                match result {
                    Ok(registration_id) => {
                        state.registration_id = Some(registration_id.clone());

                        let msg = ClientMessage::RegistrationResponse {
                            result: Ok(registration_id),
                        };

                        // drop guard before write
                        drop(_g);
                        Listener::write(client_id, msg, Arc::clone(&state.writer))
                            .await
                            .map_err(|e| {
                                error!("Failed to write message {e}");
                                if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                    client_id: myself.get_name().unwrap(),
                                    registration_id: state.registration_id.clone().unwrap(),
                                    error: Some(e.to_string()),
                                }) {
                                    error!("Failed to forwad message to handler {e}");
                                    myself.stop(Some(e.to_string()));
                                }
                            })
                            .ok();
                    }
                    Err(error) => {
                        let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {error}");
                        let msg = ClientMessage::RegistrationResponse {
                            result: Err(err_msg.clone()),
                        };

                        // drop guard before write
                        drop(_g);
                        Listener::write(client_id, msg, Arc::clone(&state.writer))
                            .await
                            .map_err(|e| {
                                error!("Failed to write message {e}");
                                if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                    client_id: myself.get_name().unwrap(),
                                    registration_id: state.registration_id.clone().unwrap(),
                                    error: Some(e.to_string()),
                                }) {
                                    myself.stop(Some(e.to_string()));
                                }
                            })
                            .ok();
                    }
                }
            }
            BrokerMessage::PublishRequest {
                topic,
                payload,
                registration_id,
                ..
            } => {
                let span = trace_span!("listener.handle_publish_request", %registration_id);
                // explicitly break context so we get a new trace for each new request
                span.set_parent(Context::new()).ok();
                let otel_ctx = span.context();
                let _g = span.enter();

                trace!(
                    "listener {client_id} received publish request for topic {topic}",
                    client_id = myself
                        .get_name()
                        .expect("Expected client to have been named.")
                );
                // listener could be unregistered when we get a request,
                // confirm listener has registered session and that the session ids match

                if state.registration_id.is_some() {
                    let listener_session_id = state.registration_id.clone().unwrap();

                    if registration_id == listener_session_id {
                        match where_is(registration_id.clone()) {
                            Some(session) => {
                                if let Err(e) =
                                    session.send_message(BrokerMessage::PublishRequest {
                                        registration_id: registration_id.clone(),
                                        topic: topic.clone(),
                                        payload: payload.clone(),
                                        trace_ctx: Some(otel_ctx),
                                    })
                                {
                                    let msg = ClientMessage::PublishResponse {
                                        topic,
                                        payload,
                                        result: Err(format!(
                                            "{PUBLISH_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: {e}"
                                        )),
                                    };

                                    drop(_g);

                                    Listener::write(
                                        state.client_id.clone(),
                                        msg,
                                        Arc::clone(&state.writer),
                                    )
                                    .await
                                    .map_err(|e| {
                                        error!("Failed to write message {e} Connection to client likely timed out.");
                                        if let Err(e) =
                                            myself.send_message(BrokerMessage::TimeoutMessage {
                                                client_id: myself.get_name().unwrap(),
                                                registration_id ,
                                                error: Some(e.to_string()),
                                            })
                                        {
                                            myself.stop(Some(e.to_string()));
                                        }
                                    })
                                    .ok();
                                    // we can consider this the end of trying to publish
                                }
                            }
                            None => {
                                // we failed to find the session, maybe it died while publishing?
                                let msg = ClientMessage::PublishResponse {
                                    topic,
                                    payload,
                                    result: Err(format!(
                                        "{PUBLISH_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}"
                                    )),
                                };

                                drop(_g);
                                Listener::write(
                                    state.client_id.clone(),
                                    msg,
                                    Arc::clone(&state.writer),
                                )
                                .await
                                .map_err(|e| {
                                    error!("Failed to write message {e}");
                                    if let Err(e) =
                                        myself.send_message(BrokerMessage::TimeoutMessage {
                                            client_id: myself.get_name().unwrap(),
                                            registration_id,
                                            error: Some(e.to_string()),
                                        })
                                    {
                                        myself.stop(Some(e.to_string()));
                                    }
                                })
                                .ok();
                            }
                        }
                    } else {
                        let err_msg =
                            format!("Received bad request, session mismatch: {registration_id:?}");
                        warn!("{err_msg}");
                        let msg = ClientMessage::ErrorMessage(err_msg);

                        drop(_g);
                        Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer))
                            .await
                            .map_err(|e| {
                                error!("Failed to write message {e}");
                                if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                    client_id: myself.get_name().unwrap(),
                                    registration_id,
                                    error: Some(e.to_string()),
                                }) {
                                    myself.stop(Some(e.to_string()));
                                }
                            })
                            .ok();
                    }
                } else {
                    let err_msg =
                        format!("Received bad request, no active session: {registration_id:?}");
                    warn!("{err_msg}");
                    let msg = ClientMessage::ErrorMessage(err_msg);

                    drop(_g);
                    Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer))
                        .await
                        .map_err(|e| {
                            error!("Failed to write message {e}");
                            if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                client_id: myself.get_name().unwrap(),
                                registration_id,
                                error: Some(e.to_string()),
                            }) {
                                myself.stop(Some(e.to_string()));
                            }
                        })
                        .ok();
                }
            }
            BrokerMessage::PublishResponse {
                topic,
                payload,
                trace_ctx,
                result,
            } => {
                let span = trace_span!("listener.dequeue_messages", %topic);
                if let Some(ctx) = trace_ctx {
                    span.set_parent(ctx).ok();
                }
                let _g = span.enter();

                trace!("listener received publish response",);

                let msg = ClientMessage::PublishResponse {
                    topic: topic.clone(),
                    payload: payload.clone(),
                    result,
                };

                drop(_g);
                Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer))
                    .await
                    .map_err(|e| {
                        error!("Failed to write message. {e}");
                        if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                            client_id: myself.get_name().unwrap(),
                            registration_id: state
                                .registration_id
                                .clone()
                                .expect("Expected to have been subscribed with a valid session"), // We wouldn't have been subscribed or published if we werent registered
                            error: Some(e.to_string()),
                        }) {
                            myself.stop(Some(e.to_string()));
                        }
                    })
                    .ok();
            }
            BrokerMessage::PublishRequestAck { topic, trace_ctx } => {
                let span = trace_span!("listener.handle_publish_request", %topic);
                if let Some(ctx) = trace_ctx {
                    span.set_parent(ctx).ok();
                }
                let _g = span.enter();

                trace!("listener received a publish request acknowledgement for topic \"{topic}\"",);

                let msg = ClientMessage::PublishRequestAck(topic);

                drop(_g);
                Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer))
                    .await
                    .map_err(|e| {
                        error!("Failed to write message {e}");
                        if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                            client_id: myself.get_name().unwrap(),
                            registration_id: state.registration_id.clone().unwrap(),
                            error: Some(e.to_string()),
                        }) {
                            myself.stop(Some(e.to_string()));
                        }
                    })
                    .ok();
            }
            BrokerMessage::SubscribeAcknowledgment {
                registration_id,
                topic,
                trace_ctx,
                result,
            } => {
                let span = trace_span!("listener.handle_subscribe_request");
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("Listener received subscribe acknolwedgement");

                let msg = ClientMessage::SubscribeAcknowledgment { topic, result };

                drop(_g);
                Listener::write(registration_id.clone(), msg, Arc::clone(&state.writer))
                    .await
                    .map_err(|e| {
                        error!("Failed to write message {e}");
                        if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                            client_id: myself.get_name().unwrap(),
                            registration_id: state.registration_id.clone().unwrap(),
                            error: Some(e.to_string()),
                        }) {
                            myself.stop(Some(e.to_string()));
                        }
                    })
                    .ok();
            }
            BrokerMessage::SubscribeRequest {
                registration_id,
                topic,
                ..
            } => {
                let span =
                    trace_span!("listener.handle_subscribe_request", %registration_id, %topic);
                // explicitly break context so we get a new trace for each new request
                span.set_parent(Context::new()).ok();
                let _g = span.enter();

                trace!("listener received subscribe request");

                //Got request to subscribe from client, confirm we've been registered
                // TODO: is there a better way to check that we're registered AND the message is coming from our actual client?
                // the connection is mTLS encrypted TCP...but I'd rather be paranoid.
                // let id = state
                //     .registration_id
                //     .clone()
                //     .take_if(|client_session_id| registration_id == client_session_id).unwrap_or_else(||
                //         todo!("Here's where we'd respond, chances are the client sent a registration_id that didn't match ours")
                //     );

                if state.registration_id.is_some()
                    && (registration_id == state.registration_id.clone().unwrap())
                {
                    let id = state.registration_id.clone().unwrap();
                    match where_is(id.clone()) {
                        Some(session) => {
                            if let Err(e) = session.send_message(BrokerMessage::SubscribeRequest {
                                registration_id: id,
                                topic,
                                trace_ctx: Some(span.context()),
                            }) {
                                let err_msg = format!(
                                    "{SUBSCRIBE_REQUEST_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: {e}"
                                );
                                error!("{err_msg}");
                                let msg = ClientMessage::ErrorMessage(err_msg);

                                drop(_g);
                                Listener::write(
                                    state.client_id.clone(),
                                    msg,
                                    Arc::clone(&state.writer),
                                )
                                .await
                                .map_err(|e| {
                                    error!("Failed to write message {e}");
                                    if let Err(e) =
                                        myself.send_message(BrokerMessage::TimeoutMessage {
                                            client_id: myself.get_name().unwrap(),
                                            registration_id: state.registration_id.clone().unwrap(),
                                            error: Some(e.to_string()),
                                        })
                                    {
                                        myself.stop(Some(e.to_string()));
                                    }
                                })
                                .ok();
                            }
                        }
                        None => {
                            error!("Could not forward request to session {id:?} ! Closing connection...");
                            let msg = ClientMessage::SubscribeAcknowledgment {
                                topic,
                                result: Result::Err(
                                    "Failed to complete request, session missing".to_string(),
                                ),
                            };

                            drop(_g);
                            Listener::write(
                                state.client_id.clone(),
                                msg,
                                Arc::clone(&state.writer),
                            )
                            .await
                            .map_err(|e| {
                                error!("Failed to write message {e}");
                                if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                    client_id: myself.get_name().unwrap(),
                                    registration_id: state.registration_id.clone().unwrap(),
                                    error: Some(e.to_string()),
                                }) {
                                    myself.stop(Some(e.to_string()));
                                }
                            })
                            .ok();

                            myself.stop(Some(SESSION_MISSING_REASON_STR.to_string()));
                        }
                    }
                } else {
                    warn!("Received bad request, session_id incorrect or not present");
                    //error back to client
                    let msg = ClientMessage::SubscribeAcknowledgment {
                        topic,
                        result: Result::Err(
                            "Bad request, session_id incorrect or not present".to_string(),
                        ),
                    };

                    drop(_g);
                    Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer))
                        .await
                        .map_err(|e| {
                            error!("Failed to write message {e}");
                            if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                client_id: myself.get_name().unwrap(),
                                registration_id,
                                error: Some(e.to_string()),
                            }) {
                                myself.stop(Some(e.to_string()));
                            }
                        })
                        .ok();
                }
            }
            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                ..
            } => {
                let span =
                    trace_span!("listener.handle_unsubscribe_request", %registration_id, %topic);
                // explicitly break context so we get a new trace for each new request
                span.set_parent(Context::new()).ok();
                let _g = span.enter();

                trace!("listener received unsubscribe request");

                if state.registration_id.is_some()
                    && (registration_id == state.registration_id.clone().unwrap())
                {
                    match where_is(registration_id.clone()) {
                        Some(session) => {
                            if let Err(e) =
                                session.send_message(BrokerMessage::UnsubscribeRequest {
                                    registration_id: registration_id.clone(),
                                    topic,
                                    trace_ctx: Some(span.context()),
                                })
                            {
                                let err_msg = format!(
                                    "{SUBSCRIBE_REQUEST_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: {e}"
                                );
                                error!("{err_msg}");
                                let msg = ClientMessage::ErrorMessage(err_msg);
                                Listener::write(
                                    state.client_id.clone(),
                                    msg,
                                    Arc::clone(&state.writer),
                                )
                                .await
                                .map_err(|e| {
                                    error!("Failed to write message {e}");
                                    if let Err(e) =
                                        myself.send_message(BrokerMessage::TimeoutMessage {
                                            client_id: myself.get_name().unwrap(),
                                            registration_id,
                                            error: Some(e.to_string()),
                                        })
                                    {
                                        myself.stop(Some(e.to_string()));
                                    }
                                })
                                .ok();
                            }
                        }
                        None => {
                            let err_msg =
                                format!("{SUBSCRIBE_REQUEST_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
                            error!("{err_msg}");
                            let msg = ClientMessage::ErrorMessage(err_msg);
                            Listener::write(
                                state.client_id.clone(),
                                msg,
                                Arc::clone(&state.writer),
                            )
                            .await
                            .map_err(|e| {
                                error!("Failed to write message {e}");
                                if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                    client_id: myself.get_name().unwrap(),
                                    registration_id,
                                    error: Some(e.to_string()),
                                }) {
                                    myself.stop(Some(e.to_string()));
                                }
                            })
                            .ok();
                        }
                    }
                } else {
                    warn!("Received request from unregistered client!! ");
                }
            }
            BrokerMessage::UnsubscribeAcknowledgment {
                registration_id,
                topic,
                ..
            } => {
                debug!("Session {registration_id} successfully unsubscribed from topic: {topic}");
                let msg = ClientMessage::UnsubscribeAcknowledgment {
                    topic,
                    result: Result::Ok(()),
                };

                let _ = Listener::write(registration_id.clone(), msg, Arc::clone(&state.writer))
                    .await
                    .map_err(|e| {
                        error!("Failed to write message {e}");
                        if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                            client_id: myself.get_name().unwrap(),
                            registration_id,
                            error: Some(e.to_string()),
                        }) {
                            myself.stop(Some(e.to_string()));
                        }
                    });
            }
            BrokerMessage::DisconnectRequest {
                client_id,
                registration_id,
                ..
            } => {
                let span = trace_span!("listener.handle_disconnect_request", %client_id);
                span.set_parent(Context::new()).ok();
                let _g = span.enter();

                trace!("Listener received disconnect request message from client.");

                if let Some(id) = registration_id {
                    //if we're registered, propagate to session agent
                    match where_is(id.clone()) {
                        Some(session) => {
                            if let Err(e) = session.send_message(BrokerMessage::DisconnectRequest {
                                client_id,
                                registration_id: Some(id.to_string()),
                                trace_ctx: Some(span.context()),
                            }) {
                                warn!("{SESSION_NOT_FOUND_TXT}: {id}: {e}");
                                myself.stop(Some("{DISCONNECTED_REASON}".to_string()));
                            }
                        }
                        None => {
                            warn!("{SESSION_NOT_FOUND_TXT}: {id}");
                            myself.stop(Some("{DISCONNECTED_REASON}".to_string()));
                        }
                    }
                } else {
                    // Otherwise, tell supervisor this listener is done
                    match myself.try_get_supervisor() {
                        Some(broker) => {
                            if let Err(e) = broker.send_message(BrokerMessage::DisconnectRequest {
                                client_id,
                                registration_id: None,
                                trace_ctx: Some(span.context()),
                            }) {
                                error!("{LISTENER_MGR_NOT_FOUND_TXT}: {e}");
                                myself.stop(Some("{DISCONNECTED_REASON}".to_string()));
                            }
                        }
                        None => {
                            error!("{LISTENER_MGR_NOT_FOUND_TXT}, ending connection");
                            myself.stop(Some("{DISCONNECTED_REASON}".to_string()));
                        }
                    }
                }
            }

            _ => {
                warn!(UNEXPECTED_MESSAGE_STR)
            }
        }
        Ok(())
    }
}
