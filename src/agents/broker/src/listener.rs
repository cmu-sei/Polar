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
};
use tokio_rustls::{server::TlsStream, TlsAcceptor};
use tracing::{debug, error, info, warn};

use async_trait::async_trait;
use ractor::{registry::where_is, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};

use crate::{
    ArchivedClientMessage, BrokerMessage, ClientMessage, BROKER_NAME, BROKER_NOT_FOUND_TXT,
    LISTENER_MGR_NOT_FOUND_TXT, PUBLISH_REQ_FAILED_TXT, REGISTRATION_REQ_FAILED_TXT,
    SESSION_MISSING_REASON_STR, SESSION_NOT_FOUND_TXT, SUBSCRIBE_REQUEST_FAILED_TXT,
    TIMEOUT_REASON,
};

use crate::UNEXPECTED_MESSAGE_STR;

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
        tracing::info!("ListenerManager: Starting {myself:?}");

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

        tracing::debug!("ListenerManager: Building configuration for mTLS ");

        let server_config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, private_key)
            .expect("bad certificate/key");

        //set up state object
        let state = ListenerManagerState {
            bind_addr: args.bind_addr,
            server_config: Arc::new(server_config),
        };
        info!("ListenerManager: Agent starting");
        Ok(state)
    }

    /// Once a the manager is running as a process, start the server
    /// and listen for incoming connections
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

        info!("ListenerManager: Server running on {bind_addr}");

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
                info!(
                    "Worker agent: {0:?}:{1:?} started",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                info!(
                    "Worker agent: {0:?}:{1:?} stopped. {reason:?}",
                    actor_cell.get_name(),
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
        match message {
            BrokerMessage::RegistrationResponse {
                registration_id,
                client_id,
                success,
                error,
            } => {
                //forwward registration_id back to listener to signal success
                debug!("Forwarding registration ack to listener: {client_id}");
                where_is(client_id.clone())
                    .unwrap()
                    .send_message(BrokerMessage::RegistrationResponse {
                        registration_id,
                        client_id,
                        success,
                        error,
                    })
                    .expect("Failed to forward message to client: {client_id}");
            }
            BrokerMessage::DisconnectRequest { client_id, .. } => {
                match where_is(client_id.clone()) {
                    Some(listener) => listener.stop(Some("{DISCONNECTED_REASON}".to_string())),
                    None => warn!("Couldn't find listener {client_id}"),
                }
            }
            BrokerMessage::TimeoutMessage { client_id, .. } => match where_is(client_id.clone()) {
                Some(listener) => listener.stop(Some(TIMEOUT_REASON.to_string())),
                None => warn!("Couldn't find listener {client_id}"),
            },
            _ => {
                todo!()
            }
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
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!(
            "Listener: Listener started for client_id: {}",
            state.client_id.clone()
        );

        let id = state.client_id.clone();

        let reader = state.reader.take().expect("Reader already taken!");

        //start listening
        let _ = tokio::spawn(async move {
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
                                        None,
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

            // Handle client disconnection, populate state in handler
            if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                client_id: String::default(),
                registration_id: None,
                error: Some("Disconnected".to_string()),
            }) {
                myself.stop(Some(e.to_string()));
            }
        });
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
            } => {
                //send message to session that it has a new listener for it to get messages from
                //forward to broker to perform any auth and potentially resume session
                match where_is(BROKER_NAME.to_string()) {
                    Some(broker) => {
                        if let Err(e) = broker.send_message(BrokerMessage::RegistrationRequest {
                            registration_id,
                            client_id: client_id.clone(),
                        }) {
                            let err_msg = format!(
                                "{REGISTRATION_REQ_FAILED_TXT}: {BROKER_NOT_FOUND_TXT}: {e}"
                            );
                            error!("{err_msg}");
                            let msg = ClientMessage::RegistrationResponse {
                                registration_id: String::default(),
                                success: false,
                                error: Some(err_msg.clone()),
                            };

                            let _ = Listener::write(client_id, msg, Arc::clone(&state.writer))
                                .await
                                .map_err(|e| error!("Failed to write message {e}"));
                            myself.stop(Some(err_msg));
                        }
                    }
                    None => {
                        let err_msg =
                            format!("{REGISTRATION_REQ_FAILED_TXT}: {BROKER_NOT_FOUND_TXT}");
                        error!("{err_msg}");
                        let msg = ClientMessage::RegistrationResponse {
                            registration_id: String::default(),
                            success: false,
                            error: Some(err_msg.clone()),
                        };

                        let _ = Listener::write(client_id, msg, Arc::clone(&state.writer))
                            .await
                            .map_err(|e| {
                                error!("Failed to write message {e}");
                                if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                    client_id: String::default(),
                                    registration_id: None,
                                    error: Some(e.to_string()),
                                }) {
                                    myself.stop(Some(e.to_string()));
                                }
                            });
                        myself.stop(Some(err_msg));
                    }
                }
            }
            BrokerMessage::RegistrationResponse {
                registration_id,
                client_id,
                success,
                error,
            } => {
                if success {
                    debug!("Successfully registered with id: {registration_id:?}");
                    state.registration_id = registration_id.clone();

                    let msg = ClientMessage::RegistrationResponse {
                        registration_id: registration_id.unwrap(),
                        success: true,
                        error: None,
                    };

                    let _ = Listener::write(client_id, msg, Arc::clone(&state.writer))
                        .await
                        .map_err(|e| {
                            error!("Failed to write message {e}");
                            if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                client_id: String::default(),
                                registration_id: None,
                                error: Some(e.to_string()),
                            }) {
                                error!("Failed to forwad message to handler {e}");
                                myself.stop(Some(e.to_string()));
                            }
                        });
                } else {
                    let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {error:?}");
                    let msg = ClientMessage::RegistrationResponse {
                        registration_id: String::default(),
                        success: false,
                        error: Some(err_msg.clone()),
                    };

                    let _ = Listener::write(client_id, msg, Arc::clone(&state.writer))
                        .await
                        .map_err(|e| {
                            error!("Failed to write message {e}");
                            if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                client_id: String::default(),
                                registration_id: None,
                                error: Some(e.to_string()),
                            }) {
                                myself.stop(Some(e.to_string()));
                            }
                        });
                }
            }
            BrokerMessage::PublishRequest {
                topic,
                payload,
                registration_id,
            } => {
                //confirm listener has registered session
                if registration_id == state.registration_id && registration_id.is_some() {
                    let id = registration_id.unwrap();
                    match where_is(id.clone()) {
                        Some(session) => {
                            if let Err(e) = session.send_message(BrokerMessage::PublishRequest {
                                registration_id: Some(id),
                                topic: topic.clone(),
                                payload: payload.clone(),
                            }) {
                                let msg = ClientMessage::PublishResponse {
                                    topic,
                                    payload,
                                    result: Err(format!(
                                        "{PUBLISH_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: {e}"
                                    )),
                                };

                                let _ = Listener::write(
                                    state.client_id.clone(),
                                    msg,
                                    Arc::clone(&state.writer),
                                )
                                .await
                                .map_err(|e| {
                                    error!("Failed to write message {e}");
                                    if let Err(e) =
                                        myself.send_message(BrokerMessage::TimeoutMessage {
                                            client_id: String::default(),
                                            registration_id: None,
                                            error: Some(e.to_string()),
                                        })
                                    {
                                        myself.stop(Some(e.to_string()));
                                    }
                                });
                            }
                        }
                        None => {
                            let msg = ClientMessage::PublishResponse {
                                topic,
                                payload,
                                result: Err(format!(
                                    "{PUBLISH_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}"
                                )),
                            };

                            let _ = Listener::write(
                                state.client_id.clone(),
                                msg,
                                Arc::clone(&state.writer),
                            )
                            .await
                            .map_err(|e| {
                                error!("Failed to write message {e}");
                                if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                    client_id: String::default(),
                                    registration_id: None,
                                    error: Some(e.to_string()),
                                }) {
                                    myself.stop(Some(e.to_string()));
                                }
                            });
                        }
                    }
                } else {
                    let err_msg =
                        format!("Received bad request, session mismatch: {registration_id:?}");
                    warn!("{err_msg}");
                    let msg = ClientMessage::ErrorMessage(err_msg);

                    let _ =
                        Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer))
                            .await
                            .map_err(|e| {
                                error!("Failed to write message {e}");
                                if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                    client_id: String::default(),
                                    registration_id: None,
                                    error: Some(e.to_string()),
                                }) {
                                    myself.stop(Some(e.to_string()));
                                }
                            });
                }
            }
            BrokerMessage::PublishResponse { topic, payload, .. } => {
                let msg = ClientMessage::PublishResponse {
                    topic: topic.clone(),
                    payload: payload.clone(),
                    result: Result::Ok(()),
                };

                let _ = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer))
                    .await
                    .map_err(|e| {
                        error!("Failed to write message {e}");
                        if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                            client_id: String::default(),
                            registration_id: None,
                            error: Some(e.to_string()),
                        }) {
                            myself.stop(Some(e.to_string()));
                        }
                    });
            }
            BrokerMessage::PublishRequestAck(topic) => {
                let msg = ClientMessage::PublishRequestAck(topic);
                let _ = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer))
                    .await
                    .map_err(|e| {
                        error!("Failed to write message {e}");
                        if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                            client_id: String::default(),
                            registration_id: None,
                            error: Some(e.to_string()),
                        }) {
                            myself.stop(Some(e.to_string()));
                        }
                    });
            }
            BrokerMessage::SubscribeAcknowledgment {
                registration_id,
                topic,
                ..
            } => {
                debug!("Agent successfully subscribed to topic: {topic}");
                let msg = ClientMessage::SubscribeAcknowledgment {
                    topic,
                    result: Result::Ok(()),
                };

                let _ = Listener::write(registration_id.clone(), msg, Arc::clone(&state.writer))
                    .await
                    .map_err(|e| {
                        error!("Failed to write message {e}");
                        if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                            client_id: String::default(),
                            registration_id: None,
                            error: Some(e.to_string()),
                        }) {
                            myself.stop(Some(e.to_string()));
                        }
                    });
            }
            BrokerMessage::SubscribeRequest {
                registration_id,
                topic,
            } => {
                //Got request to subscribe from client, confirm we've been registered
                if registration_id == state.registration_id && registration_id.is_some() {
                    let id = registration_id.unwrap();
                    match where_is(id.clone()) {
                        Some(session) => {
                            if let Err(e) = session.send_message(BrokerMessage::SubscribeRequest {
                                registration_id: Some(id),
                                topic,
                            }) {
                                let err_msg = format!(
                                    "{SUBSCRIBE_REQUEST_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: {e}"
                                );
                                error!("{err_msg}");
                                let msg = ClientMessage::ErrorMessage(err_msg);

                                let _ = Listener::write(
                                    state.client_id.clone(),
                                    msg,
                                    Arc::clone(&state.writer),
                                )
                                .await
                                .map_err(|e| {
                                    error!("Failed to write message {e}");
                                    if let Err(e) =
                                        myself.send_message(BrokerMessage::TimeoutMessage {
                                            client_id: String::default(),
                                            registration_id: None,
                                            error: Some(e.to_string()),
                                        })
                                    {
                                        myself.stop(Some(e.to_string()));
                                    }
                                });
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

                            let _ = Listener::write(
                                state.client_id.clone(),
                                msg,
                                Arc::clone(&state.writer),
                            )
                            .await
                            .map_err(|e| {
                                error!("Failed to write message {e}");
                                if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                    client_id: String::default(),
                                    registration_id: None,
                                    error: Some(e.to_string()),
                                }) {
                                    myself.stop(Some(e.to_string()));
                                }
                            });
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

                    let _ =
                        Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer))
                            .await
                            .map_err(|e| {
                                error!("Failed to write message {e}");
                                if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                    client_id: String::default(),
                                    registration_id: None,
                                    error: Some(e.to_string()),
                                }) {
                                    myself.stop(Some(e.to_string()));
                                }
                            });
                }
            }
            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
            } => {
                if registration_id == state.registration_id && registration_id.is_some() {
                    let id = registration_id.unwrap();
                    match where_is(id.clone()) {
                        Some(session) => {
                            if let Err(e) =
                                session.send_message(BrokerMessage::UnsubscribeRequest {
                                    registration_id: Some(id),
                                    topic,
                                })
                            {
                                let err_msg = format!(
                                    "{SUBSCRIBE_REQUEST_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: {e}"
                                );
                                error!("{err_msg}");
                                let msg = ClientMessage::ErrorMessage(err_msg);
                                let _ = Listener::write(
                                    state.client_id.clone(),
                                    msg,
                                    Arc::clone(&state.writer),
                                )
                                .await
                                .map_err(|e| {
                                    error!("Failed to write message {e}");
                                    if let Err(e) =
                                        myself.send_message(BrokerMessage::TimeoutMessage {
                                            client_id: String::default(),
                                            registration_id: None,
                                            error: Some(e.to_string()),
                                        })
                                    {
                                        myself.stop(Some(e.to_string()));
                                    }
                                });
                            }
                        }
                        None => {
                            let err_msg =
                                format!("{SUBSCRIBE_REQUEST_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
                            error!("{err_msg}");
                            let msg = ClientMessage::ErrorMessage(err_msg);
                            let _ = Listener::write(
                                state.client_id.clone(),
                                msg,
                                Arc::clone(&state.writer),
                            )
                            .await
                            .map_err(|e| {
                                error!("Failed to write message {e}");
                                if let Err(e) = myself.send_message(BrokerMessage::TimeoutMessage {
                                    client_id: String::default(),
                                    registration_id: None,
                                    error: Some(e.to_string()),
                                }) {
                                    myself.stop(Some(e.to_string()));
                                }
                            });
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
                            client_id: String::default(),
                            registration_id: None,
                            error: Some(e.to_string()),
                        }) {
                            myself.stop(Some(e.to_string()));
                        }
                    });
            }
            BrokerMessage::DisconnectRequest {
                client_id,
                registration_id,
            } => {
                info!("Client {client_id} disconnected. Ending Session");
                if registration_id == state.registration_id && registration_id.is_some() {
                    info!("Client {client_id} disconnected.");
                    //if we're registered, propagate to session agent

                    let id = registration_id.unwrap();
                    match where_is(id.clone()) {
                        Some(session) => session
                            .send_message(BrokerMessage::DisconnectRequest {
                                client_id,
                                registration_id: Some(id.to_string()),
                            })
                            .map_err(|e| {
                                warn!("{SESSION_NOT_FOUND_TXT}: {id}: {e}");
                                myself.stop(Some("{DISCONNECTED_REASON}".to_string()));
                            })
                            .unwrap(),
                        None => {
                            warn!("{SESSION_NOT_FOUND_TXT}: {id}");
                            myself.stop(Some("{DISCONNECTED_REASON}".to_string()));
                        }
                    }
                } else {
                    // Otherwise, tell supervisor this listener is done
                    match myself.try_get_supervisor() {
                        Some(broker) => broker
                            .send_message(BrokerMessage::DisconnectRequest {
                                client_id,
                                registration_id: None,
                            })
                            .map_err(|e| {
                                error!("{LISTENER_MGR_NOT_FOUND_TXT}: {e}");
                                myself.stop(Some("{DISCONNECTED_REASON}".to_string()));
                            })
                            .unwrap(),

                        None => {
                            error!("{LISTENER_MGR_NOT_FOUND_TXT}, ending connection");
                            myself.stop(Some("{DISCONNECTED_REASON}".to_string()));
                        }
                    }
                }
            }
            BrokerMessage::TimeoutMessage { error, .. } => {
                //Client timed out, if we were registered, let session know
                match &state.registration_id {
                    Some(id) => where_is(id.to_owned()).map_or_else(
                        || {},
                        |session| {
                            warn!("{myself:?} disconnected unexpectedly!");
                            session
                                .send_message(BrokerMessage::TimeoutMessage {
                                    client_id: myself.get_name().unwrap(),
                                    registration_id: Some(id.clone()),
                                    error: error,
                                })
                                .map_err(|e| {
                                    warn!("{SESSION_NOT_FOUND_TXT}: {id}: {e}");
                                })
                                .unwrap();
                            myself.stop(Some(TIMEOUT_REASON.to_string()));
                        },
                    ),
                    _ => (),
                }
                myself.stop(Some(TIMEOUT_REASON.to_string()));
            }
            _ => {
                warn!(UNEXPECTED_MESSAGE_STR)
            }
        }
        Ok(())
    }
}
