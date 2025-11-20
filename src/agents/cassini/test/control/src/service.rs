use harness_common::{
    Action, Agent, ArchivedHarnessControllerMessage, HarnessControllerMessage, ProducerConfig,
    TestPlan,
};
use oci_client::client::ClientConfig;
use ractor::{
    async_trait, cast,
    concurrency::{sleep, Duration, JoinHandle},
    registry::where_is,
    Actor, ActorProcessingErr, ActorRef, SupervisionEvent,
};
use rkyv::{
    deserialize,
    rancor::{self, Error, Source},
};
use rustls::{
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
    server::WebPkiClientVerifier,
    RootCertStore, ServerConfig,
};
use serde_json::to_string_pretty;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use testcontainers::{
    core::{IntoContainerPort, Mount, PortMapping},
    ContainerAsync, GenericImage,
};
use testcontainers::{GenericBuildableImage, ImageExt};
use testcontainers_modules::{
    neo4j::{Neo4j, Neo4jImage},
    testcontainers::{runners::AsyncRunner, BuildableImage, Image},
};

use tokio::{
    io::{split, AsyncReadExt, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task::AbortHandle,
};
use tokio_rustls::{server::TlsStream, TlsAcceptor};
use tracing::{debug, error, info, warn, Instrument};

use crate::{
    images::{self, AgentImage, CassiniImage, CA_CERT, SERVER_CERTIFICATE, SERVER_KEY, TLS_PATH},
    spawn_broker,
};

pub const SINK_CLIENT_SESSION: &str = "cassini.harness.session";

/// TODO: We should likely separate the agent handlers and maybe even cassini from the underlying infra,
/// this could in turn become referred to as the BASE infra, where all other agents would be considered Infra under test
pub struct TestInfra {
    cassini: Option<ContainerAsync<CassiniImage>>,
    /// an optional but nice to have OCI registry,
    registry: Option<ContainerAsync<GenericImage>>,
    neo4j: Option<ContainerAsync<Neo4jImage>>,
}

impl TestInfra {
    /// Starts the test infrastructure according to a given TestPlan, should return an instance of the infrastructure struct that holds
    /// the refs to running testcontainers.
    /// Any failure to start a container is treated as a startup error and passed back to the supervisor.
    pub async fn start_environment(plan: TestPlan) -> Result<Self, ActorProcessingErr> {
        let mut state = TestInfra {
            cassini: None,
            registry: None,
            neo4j: None,
        };

        //
        // Broker
        let cassini = plan.environment.cassini;

        info!("Starting cassini server image version: {}", cassini.tag);

        let ca_cert_path = format!("{}/{}", TLS_PATH, CA_CERT);
        let server_cert_path = format!("{}/{}", TLS_PATH, SERVER_CERTIFICATE);
        let server_key_path = format!("{}/{}", TLS_PATH, SERVER_KEY);

        match CassiniImage::new(cassini.tag)
            .with_container_name("polar-harness-cassini")
            // Mount files into container
            .with_mount(Mount::bind_mount(&cassini.ca_cert_path, &ca_cert_path))
            .with_mount(Mount::bind_mount(
                &cassini.server_cert_path,
                &server_cert_path,
            ))
            .with_mount(Mount::bind_mount(
                &cassini.server_key_path,
                &server_key_path,
            ))
            // Inject environment variables
            .with_env_var("TLS_CA_CERT", &ca_cert_path)
            .with_env_var("TLS_SERVER_CERT_CHAIN", &server_cert_path)
            .with_env_var("TLS_SERVER_KEY", &server_key_path)
            // default to debug level output
            .with_env_var(
                "RUST_LOG",
                cassini.log_level.unwrap_or_else(|| "debug".to_string()),
            )
            .with_mapped_port(8080, 8080.tcp())
            .with_mapped_port(3000, 3000.tcp())
            // .with_reuse(testcontainers::ReuseDirective::Always)
            .start()
            .in_current_span()
            .await
        {
            Ok(cassini) => state.cassini = Some(cassini),
            Err(e) => return Err(ActorProcessingErr::from(e)),
        }

        //
        // NEO4J
        //
        if plan.environment.neo4j.enable {
            debug!("Starting neo4j {}", plan.environment.neo4j.version);

            // Neo4j passwords can and do contain / if users misconfigure them.
            // split('/') would fragment it into too many pieces
            let (user, password) = match std::env::var("NEO4J_AUTH") {
                Ok(auth_str) => {
                    let mut parts = auth_str.splitn(2, '/');
                    match (parts.next(), parts.next()) {
                        (Some(u), Some(p)) => (u.to_string(), p.to_string()),
                        _ => {
                            error!("NEO4J_AUTH must be in form user/pass");
                            todo!("return error");
                        }
                    }
                }
                Err(_) => {
                    error!("NEO4J_AUTH missing");
                    todo!("return error");
                }
            };

            // let db_ports = vec![7474.tcp(), 7687.tcp()];

            // TODO: This seems to always use the community version from docker.io, not ideal for our purposes.
            // switch to Generic Image and passing a ref to a secured registry is better.
            match Neo4j::new()
                .with_user(user)
                .with_password(password)
                .with_container_name(format!(
                    "polar-harness-neo4j-{}",
                    plan.environment.neo4j.version
                ))
                .with_mapped_port(7474, 7474.tcp())
                .with_mapped_port(7687, 7687.tcp())
                // .with_reuse(testcontainers::ReuseDirective::Always)
                .start()
                .await
            {
                Ok(neo4j) => {
                    info!("Neo4j:{} started.", plan.environment.neo4j.version);
                    state.neo4j = Some(neo4j);
                }
                Err(e) => return Err(ActorProcessingErr::from(e)),
            }
        }

        //
        // REGISTRY
        //
        if plan.environment.registry.enable {
            info!("Starting local OCI registry...");
            // TODO: make configurable?
            const REGISTRY_PORT: u16 = 5000;

            let req = GenericImage::new("registry", "3.0.0")
                .with_container_name("polar-test-registry")
                // .with_reuse(testcontainers::ReuseDirective::Always)
                .with_mapped_port(REGISTRY_PORT, REGISTRY_PORT.tcp());

            match req.start().await {
                Ok(registry) => {
                    use oci_client::{
                        client::{Config, ImageLayer},
                        manifest::OciImageManifest,
                        secrets::RegistryAuth,
                        Client, Reference,
                    };
                    use serde_json::json;

                    info!("in-memory registry started");
                    // upload a simple OCI artifact

                    let reference: Reference =
                        format!("localhost:{REGISTRY_PORT}/polar/test-artifact:latest")
                            .parse()
                            .unwrap();

                    let client = Client::new(ClientConfig {
                        protocol: oci_client::client::ClientProtocol::Http, // <- critical, we're not bothering with ssl
                        accept_invalid_certificates: true, // irrelevant since no TLS, but safe
                        accept_invalid_hostnames: true,
                        ..Default::default()
                    });

                    // Create a trivial JSON payload
                    let payload = json!({
                        "hello": "world",
                        "value": 1,
                    });
                    let bytes = serde_json::to_vec(&payload).unwrap();

                    let config = Config {
                        data: bytes.clone(),
                        media_type: "application/vnd.polar.test+json".to_string(),
                        annotations: None,
                    };

                    // Push into registry
                    let layers = vec![ImageLayer::new(
                        bytes,
                        "application/vnd.polar.test+json".to_string(),
                        None,
                    )];

                    let image_manifest = OciImageManifest::build(
                        &layers, &config, None, /* optional annotations */
                    );
                    debug!("uploading test artifacts...");
                    client
                        .push(
                            &reference,
                            &layers,
                            config,
                            &RegistryAuth::Anonymous,
                            Some(image_manifest),
                        )
                        .await
                        .expect("push should succeed");

                    info!("pushed test artifact to {}", reference);

                    state.registry = Some(registry);
                }
                Err(e) => {
                    error!("Error starting container registry.");
                    return Err(ActorProcessingErr::from(e));
                }
            };
        }
        Ok(state)
    }

    /// Yoink
    /// Take each container out of the Option and drop it
    /// Whenever we decide we want to start and manage other agents, we'll take those too.
    pub async fn stop(&mut self) {
        self.cassini.take();
        self.neo4j.take();
        self.registry.take();

        // Containers die when the guards are dropped
        info!("TestInfra stopped, containers cleaned up");
    }
}

pub struct TestState {
    producer_config: Option<ProducerConfig>,
    /// which agents are actively being used by the controller in a given phase.
    agents: Option<HashMap<String, ContainerAsync<AgentImage>>>,
}

impl TestState {
    pub async fn start_agent(&mut self, agent: &Agent) -> Result<(), ActorProcessingErr> {
        let image = AgentImage::from_agent(agent);
        let name = image.name.clone();
        debug!("Created image\n{agent:?}");
        info!("Starting agent image: {}", image.name);

        return match image.start().await {
            Ok(c) => {
                if let Some(agents) = self.agents.as_mut() {
                    agents.insert(name.clone(), c).inspect(|old_container| {
                        info!("Replacing container: {}", old_container.image().name);
                    });
                    Ok(())
                } else {
                    let mut agents = HashMap::new();
                    agents.insert(name, c);
                    self.agents = Some(agents);
                    Ok(())
                }
            }
            Err(e) => return Err(ActorProcessingErr::from(e)),
        };
    }

    /// Yoink
    /// Take each container out of the Option and drop it
    /// Whenever we decide we want to start and manage other agents, we'll take those too.
    pub async fn stop_agent(&mut self) {
        // Containers die when the guards are dropped
        info!("TestInfra stopped, containers cleaned up");
    }
}
// ================ ============== Sink Service ============================== //

pub struct HarnessController;

pub struct HarnessControllerState {
    bind_addr: String,
    server_config: ServerConfig,
    pub test_plan: TestPlan,
    server_handle: Option<AbortHandle>,
    infra: TestInfra,
    test_state: TestState,
}

pub struct HarnessControllerArgs {
    pub bind_addr: String,
    pub server_cert_file: String,
    pub private_key_file: String,
    pub ca_cert_file: String,
    pub test_plan: TestPlan,
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

        // start infrastructure based on test plan
        let infra = TestInfra::start_environment(args.test_plan.clone())
            .await
            .expect("Expected infra to start");

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
            infra,
            test_state: TestState {
                producer_config: None,
                agents: None,
            },
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

                        // send them a registration id so we know how to look them up later.
                        let client_id = uuid::Uuid::new_v4().to_string();

                        info!("Client: {client_id} registered.");

                        client
                            .send_message(ClientSessionMessage::Send(
                                HarnessControllerMessage::ClientRegistered { client_id },
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

        // once we've started our env, we need to start standing up agents
        for phase in &state.test_plan.phases {
            // for each phase, run a test, which effectively breaks down to a series of actions.
            // each one can be sort of a BDD situation.
            info!("Starting phase {}", phase.name);

            for action in &phase.actions {
                match action {
                    Action::StartAgent { agent, config } => {
                        if let Err(e) = state.test_state.start_agent(&agent.clone()).await {
                            error!("failed to start agent. {e}");
                            return Err(ActorProcessingErr::from(e));
                        }
                        // if agent is of type producer, read its config and save in state.
                        if let Agent::ProducerAgent = agent {
                            if let Some(path) = config {
                                match std::fs::read_to_string(path) {
                                    Ok(dhall_str) => {
                                        match serde_dhall::from_str(&dhall_str)
                                            .parse::<ProducerConfig>()
                                        {
                                            Ok(config) => {
                                                info!(
                                                    "Read producer config:\n{}",
                                                    to_string_pretty(&config).unwrap()
                                                );
                                                // set config in test state so we can use it later.
                                                state.test_state.producer_config = Some(config);
                                            }
                                            Err(e) => return Err(ActorProcessingErr::from(e)),
                                        }
                                    }
                                    Err(e) => {
                                        todo!()
                                    }
                                }
                            }
                        }
                    }
                    Action::AssertEvent { topic, count } => {
                        todo!()
                    }
                    _ => todo!("handle additonal actions"),
                }
            }
        }
        //
        // wait for a little then shutdown
        //
        sleep(Duration::from_secs(3)).await;

        myself.cast(HarnessControllerMessage::ShutdownAck).ok();

        Ok(())
    }

    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Abort any running child handles
        if let Some(handle) = state.server_handle.as_ref() {
            handle.abort();
        }

        state.infra.stop().await;

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
            HarnessControllerMessage::ShutdownAck => myself.stop(Some("TEST_COMPLETE".to_string())),
            HarnessControllerMessage::ProducerReady { client_id } => {
                // read producer config

                info!("dispensing configuration to producer {client_id}");

                if let Some(config) = &state.test_state.producer_config {
                    //send
                    match where_is(client_id) {
                        Some(client) => {
                            let msg = HarnessControllerMessage::StartProducers {
                                producers: config.producers.clone(),
                            };
                            cast!(ActorRef::from(client), ClientSessionMessage::Send(msg)).ok();
                        }
                        None => {
                            return Err(ActorProcessingErr::from(
                                "Failed to locate client session actor.",
                            ))
                        }
                    }
                }
            }
            HarnessControllerMessage::SinkReady { client_id } => {
                info!("dispensing configuration to sink {client_id}");

                if let Some(config) = &state.test_state.producer_config {
                    //send
                    match where_is(client_id) {
                        Some(client) => {
                            let topics = config
                                .producers
                                .iter()
                                .map(|p| p.topic.clone())
                                .collect::<Vec<String>>();

                            let msg = HarnessControllerMessage::StartSinks { topics };
                            cast!(ActorRef::from(client), ClientSessionMessage::Send(msg)).ok();
                        }
                        None => {
                            return Err(ActorProcessingErr::from(
                                "Failed to locate client session actor.",
                            ))
                        }
                    }
                }
            }
            _ => todo!("Handle message {message:?}"),
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
