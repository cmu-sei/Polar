/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

pub use cassini_tracing::{init_tracing, shutdown_tracing};
pub use cassini_tracing::{try_set_parent_otel, try_set_parent_wire};
mod connect;
pub mod environment;
pub mod facade;
pub mod refused;

use cassini_types::{ClientEvent, ClientMessage, ControlOp, WireTraceCtx};
use ractor::concurrency::{Duration, sleep};
use ractor::{Actor, ActorProcessingErr, ActorRef, Message, OutputPort, RpcReplyPort, async_trait};
use rkyv::rancor::Error;
use rustls::ClientConfig;
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use socket2::SockRef;
use std::env;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncReadExt, BufWriter, ReadHalf, WriteHalf, split};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::task::AbortHandle;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tracing::{
    Instrument, debug, debug_span, error, info, info_span, trace, trace_span, warn, warn_span,
};

pub mod cli;

pub const UNEXPECTED_DISCONNECT: &str = "UNEXPECTED_DISCONNECT";
pub const REGISTRATION_EXPECTED: &str =
    "Expected client to be registered to conduct this operation.";

pub const CLIENT_DISCONNECTED: &str = "CLIENT_DISCONNECTED";

/// Bridges [`ClientEvent`] messages from the TCP client to any actor using a mapping closure.
///
/// This is the recommended way to receive events from [`TcpClientActor`]. Unlike subscribing
/// to the [`OutputPort`], this uses ractor's normal mpsc mailbox and will never silently drop
/// messages under load. See: https://github.com/slawlor/ractor/issues/225
pub struct ClientEventForwarder<M: Message + Sync>(PhantomData<M>);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueueEntry {
    pub topic: String,
    pub payload_b64: String,
    pub timestamp: String,
    pub attempts: u32,
}

#[derive(Debug, Clone, Default)]
pub enum OfflineBehavior {
    #[default]
    Queue,
    Drop,
    Fail,
}

#[derive(Clone)]
pub struct MessageQueue {
    pub path: std::path::PathBuf,
}

impl MessageQueue {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn from_env() -> Option<Self> {
        environment::CASSINI_QUEUE_PATH
            .read(&polar_env::ProcessEnv)
            .ok()
            .flatten()
            .map(|p| Self::new(PathBuf::from(p)))
    }

    pub fn append(&self, entry: &QueueEntry) -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        writeln!(file, "{}", line)
    }

    pub fn drain(&self) -> std::io::Result<Vec<QueueEntry>> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let file = std::fs::File::open(&self.path)?;
        let reader = std::io::BufReader::new(file);
        let entries = std::io::BufRead::lines(reader)
            .filter_map(|line| {
                line.ok()
                    .and_then(|l| serde_json::from_str::<QueueEntry>(&l).ok())
            })
            .collect();
        std::fs::remove_file(&self.path)?;
        Ok(entries)
    }

    pub fn len(&self) -> std::io::Result<usize> {
        if !self.path.exists() {
            return Ok(0);
        }
        let file = std::fs::File::open(&self.path)?;
        let reader = std::io::BufReader::new(file);
        Ok(std::io::BufRead::lines(reader).count())
    }

    pub fn is_empty(&self) -> std::io::Result<bool> {
        Ok(self.len()? == 0)
    }

    pub fn clear(&self) -> std::io::Result<()> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct PublishRequest {
    pub topic: String,
    pub payload: Vec<u8>,
    pub trace_ctx: Option<WireTraceCtx>,
    pub offline_behavior: OfflineBehavior,
    pub share_identity: bool,
}

impl<M: Message + Sync> Default for ClientEventForwarder<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Message + Sync> ClientEventForwarder<M> {
    pub fn new() -> Self {
        ClientEventForwarder(PhantomData)
    }
}

pub struct ClientEventForwarderArgs<M: Message + Sync> {
    pub target: ActorRef<M>,
    pub mapper: Box<dyn Fn(ClientEvent) -> Option<M> + Send + Sync + 'static>,
}

pub struct ClientEventForwarderState<M: Message + Sync> {
    target: ActorRef<M>,
    mapper: Box<dyn Fn(ClientEvent) -> Option<M> + Send + Sync + 'static>,
}

#[async_trait]
impl<M: Message + Sync> Actor for ClientEventForwarder<M> {
    type Msg = ClientEvent;
    type State = ClientEventForwarderState<M>;
    type Arguments = ClientEventForwarderArgs<M>;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(ClientEventForwarderState {
            target: args.target,
            mapper: args.mapper,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let Some(mapped) = (state.mapper)(msg)
            && let Err(e) = state.target.send_message(mapped)
        {
            warn!("ClientEventForwarder: target actor unreachable: {e}");
        }
        Ok(())
    }
}

/// What a transport does when its connection to the broker is lost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReconnectMode {
    /// Connect again and ask to resume, inside the same actor. The transport on its own (the CLI
    /// daemon, and every agent that holds it directly) has always done this.
    #[default]
    InPlace,
    /// End, and leave bringing up a new transport to whoever supervises it. The facade does:
    /// it knows whether a transport is registered, holds publishes meanwhile, and backs off. A
    /// transport that mended itself would leave it believing the connection was still up.
    LeaveToSupervisor,
}

/// A basse configuration for a TCP Client actor
#[derive(Clone, Debug)]
pub struct TCPClientConfig {
    pub broker_endpoint: String,
    pub server_name: String,
    pub ca_certificate_path: String,
    pub client_certificate_path: String,
    pub client_key_path: String,
    /// What the transport does when the connection is lost.
    pub reconnect: ReconnectMode,
}

#[derive(Debug)]
pub enum RegistrationState {
    Unregistered,
    Registered { registration_id: String },
}

impl TCPClientConfig {
    /// Construct directly from values — used in tests and anywhere the
    /// caller already has the coordinates without needing env vars.
    pub fn from_values(
        broker_endpoint: String,
        server_name: String,
        ca_certificate_path: String,
        client_certificate_path: String,
        client_key_path: String,
    ) -> Self {
        TCPClientConfig {
            broker_endpoint,
            server_name,
            ca_certificate_path,
            client_certificate_path,
            client_key_path,
            reconnect: ReconnectMode::default(),
        }
    }

    /// This configuration, with the transport to do `reconnect` when the connection is lost.
    pub fn with_reconnect(mut self, reconnect: ReconnectMode) -> Self {
        self.reconnect = reconnect;
        self
    }

    /// Read the coordinates from the process environment. If we can't read these, we can't
    /// start, and the error names every variable that is missing.
    pub fn new() -> Result<Self, ActorProcessingErr> {
        Self::from_env(&polar_env::ProcessEnv).map_err(ActorProcessingErr::from)
    }

    /// As [`new`](Self::new), reading from `env`, so a snapshot or a test supplies its own.
    /// Every problem is reported together, each naming the variable and what it is for.
    pub fn from_env(env: &dyn polar_env::Env) -> Result<Self, String> {
        use environment::{
            BROKER_ADDR, CASSINI_SERVER_NAME, TLS_CA_CERT, TLS_CLIENT_CERT, TLS_CLIENT_KEY,
        };
        let mut problems = Vec::new();
        let mut take = |read: Result<String, polar_env::EnvError>| match read {
            Ok(value) => value,
            Err(error) => {
                problems.push(error.to_string());
                String::new()
            }
        };
        let client_certificate_path = take(TLS_CLIENT_CERT.read(env));
        let client_key_path = take(TLS_CLIENT_KEY.read(env));
        let ca_certificate_path = take(TLS_CA_CERT.read(env));
        let broker_endpoint = take(BROKER_ADDR.read(env));
        let server_name = take(CASSINI_SERVER_NAME.read(env));
        if !problems.is_empty() {
            return Err(problems.join("; "));
        }
        Ok(TCPClientConfig {
            broker_endpoint,
            server_name,
            ca_certificate_path,
            client_certificate_path,
            client_key_path,
            reconnect: ReconnectMode::default(),
        })
    }

    pub fn from_cli_or_env(
        broker_addr: Option<String>,
        ca_cert: Option<String>,
        client_cert: Option<String>,
        client_key: Option<String>,
        server_name: Option<String>,
    ) -> Option<Self> {
        let broker_endpoint = broker_addr.or_else(|| env::var("BROKER_ADDR").ok())?;
        let server_name = server_name.or_else(|| env::var("CASSINI_SERVER_NAME").ok())?;
        let ca_certificate_path = ca_cert.or_else(|| env::var("TLS_CA_CERT").ok())?;
        let client_certificate_path = client_cert.or_else(|| env::var("TLS_CLIENT_CERT").ok())?;
        let client_key_path = client_key.or_else(|| env::var("TLS_CLIENT_KEY").ok())?;

        Some(TCPClientConfig {
            broker_endpoint,
            server_name,
            ca_certificate_path,
            client_certificate_path,
            client_key_path,
            reconnect: ReconnectMode::default(),
        })
    }
}

/// Messages handled by the TCP client actor
#[derive(Debug)]
pub enum TcpClientMessage {
    // Send(ClientMessage),
    RegistrationResponse(String),
    ErrorMessage(String),
    /// Returns the registration id when queried, unlikely to be useful
    /// unless some other agent dies before the client does and needs to get it back
    GetRegistrationId(RpcReplyPort<Option<String>>),
    /// Message that gets emitted when the client successfully registers with the broker.
    /// It should contain a valid registration uid
    ClientRegistered(String),
    // attempt to register with the broker, optionally with a registration id to restart a session.
    Register,
    /// Publish request from the client.
    Publish {
        topic: String,
        payload: Vec<u8>,
        trace_ctx: Option<WireTraceCtx>,
        /// Whether the publisher's identity (always known to the
        /// broker via the connection's own verified mTLS cert) should
        /// actually be forwarded to subscribers on delivery. Prefer
        /// the publish()/publish_anonymous() constructors over
        /// constructing this variant directly.
        share_identity: bool,
    },
    Subscribe {
        topic: String,
        trace_ctx: Option<WireTraceCtx>,
    },
    /// Unsubscribe request from the client.
    /// Contains the topic
    UnsubscribeRequest {
        topic: String,
        trace_ctx: Option<WireTraceCtx>,
    },
    ///Disconnect, sending a session id to end, if any
    Disconnect {
        trace_ctx: Option<WireTraceCtx>,
    },
    /// Control plane operations
    ControlRequest {
        op: cassini_types::ControlOp,
        trace_ctx: Option<WireTraceCtx>,
    },
    ListSessions {
        trace_ctx: Option<WireTraceCtx>,
    },
    ListTopics {
        trace_ctx: Option<WireTraceCtx>,
    },
    GetSession {
        trace_ctx: Option<WireTraceCtx>,
    },
    Reconnect,
    Reconnected {
        reader: Option<ReadHalf<TlsStream<TcpStream>>>,
        writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    },
    /// Sent when the broker rejects a session-resume attempt (e.g. after
    /// a broker restart/rejuvenation cleared its session table). Forces
    /// a transition back to Unregistered and a fresh registration,
    /// rather than retrying the same now-invalid registration_id forever.
    SessionInvalidated,
    /// From the transport to the facade: the broker's answer to a subscribe or a publish this client sent (`Ok`
    /// only for a subscription taken). The facade keeps what was refused (`refused`).
    BrokerAnswered {
        topic: String,
        use_: facade::TopicUse,
        result: Result<(), String>,
    },
    /// The agent's authority changed: the facade asks its gate again about every topic it was refused, and sends
    /// the wanted subscriptions it now admits.
    JudgeAgain,
}

impl TcpClientMessage {
    /// The message's kind, its variant's name: what a span records of it. Its Debug would write out a publish's
    /// payload.
    pub fn kind(&self) -> &'static str {
        match self {
            TcpClientMessage::RegistrationResponse { .. } => "RegistrationResponse",
            TcpClientMessage::ErrorMessage { .. } => "ErrorMessage",
            TcpClientMessage::GetRegistrationId { .. } => "GetRegistrationId",
            TcpClientMessage::ClientRegistered { .. } => "ClientRegistered",
            TcpClientMessage::Register => "Register",
            TcpClientMessage::Publish { .. } => "Publish",
            TcpClientMessage::Subscribe { .. } => "Subscribe",
            TcpClientMessage::UnsubscribeRequest { .. } => "UnsubscribeRequest",
            TcpClientMessage::Disconnect { .. } => "Disconnect",
            TcpClientMessage::ControlRequest { .. } => "ControlRequest",
            TcpClientMessage::ListSessions { .. } => "ListSessions",
            TcpClientMessage::ListTopics { .. } => "ListTopics",
            TcpClientMessage::GetSession { .. } => "GetSession",
            TcpClientMessage::Reconnect => "Reconnect",
            TcpClientMessage::Reconnected { .. } => "Reconnected",
            TcpClientMessage::SessionInvalidated => "SessionInvalidated",
            TcpClientMessage::BrokerAnswered { .. } => "BrokerAnswered",
            TcpClientMessage::JudgeAgain => "JudgeAgain",
        }
    }

    /// The common case -- identity shared with subscribers. Prefer
    /// this over constructing Publish directly.
    pub fn publish(topic: String, payload: Vec<u8>, trace_ctx: Option<WireTraceCtx>) -> Self {
        TcpClientMessage::Publish {
            topic,
            payload,
            trace_ctx,
            share_identity: true,
        }
    }

    /// Explicit opt-out for the rare case where genuine anonymity is
    /// wanted -- the broker still knows who's connected (it's derived
    /// from the verified mTLS cert, not client-supplied), it simply
    /// won't forward that identity on to subscribers for this publish.
    pub fn publish_anonymous(
        topic: String,
        payload: Vec<u8>,
        trace_ctx: Option<WireTraceCtx>,
    ) -> Self {
        TcpClientMessage::Publish {
            topic,
            payload,
            trace_ctx,
            share_identity: false,
        }
    }
}

type TlsWriter = Option<Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>>;

/// How a `TcpClientActor` delivers every real, incoming `ClientEvent`
/// to the rest of its own agent -- the one, real, structural choice
/// this crate genuinely offers, replacing what were previously two,
/// independent, always-both-fired fields (`events_output` broadcast
/// and a direct `event_handler`, see this crate's own, prior history
/// for the shape this replaced).
///
/// `Broadcast` is unchanged, existing behavior -- every current agent
/// keeps working exactly as before, migrated mechanically to this
/// enum's own shape.
///
/// `Router` is the new, real, per-topic-directed alternative: genuine
/// `ClientEvent::MessagePublished` events are converted to a real
/// `polar_router::IncomingMessage` and routed by topic via `router`
/// (see `polar-router`'s own crate documentation for the complete,
/// real reasoning); every other, real `ClientEvent` variant --
/// connection-lifecycle events like `Registered`/`TransportError`,
/// never genuinely "a message on a topic" in the first place -- goes
/// directly to `lifecycle` instead, mirroring `TcpClient::spawn`'s
/// own, existing `map_event` closure pattern.
#[derive(Clone)]
pub enum DeliveryMode {
    Broadcast(Arc<OutputPort<ClientEvent>>),
    Router {
        router: ActorRef<polar_router::RouterMessage>,
        // Arc, not Box -- spawn_reader's own real, spawned read-loop
        // task needs its own, cloned copy of `delivery` (the same
        // real reason state.events_output/event_handler were always
        // cloned out of state before this replaced them), and a
        // trait-object closure can only be cloned via shared
        // ownership, never by value.
        lifecycle: Arc<dyn Fn(ClientEvent) + Send + Sync>,
    },
}

/// Actor state for the TCP client
pub struct TcpClientState {
    bind_addr: String,
    server_name: String,
    writer: TlsWriter,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>, // Use Option to allow taking ownership
    registration: RegistrationState,
    client_config: Arc<ClientConfig>,

    delivery: DeliveryMode,
    abort_handle: Option<AbortHandle>, // abort handle switch for the stream read loop
    reconnect: ReconnectMode,
    answers: Option<ActorRef<TcpClientMessage>>,
}

pub struct TcpClientArgs {
    pub config: TCPClientConfig,
    pub registration_id: Option<String>,
    pub delivery: DeliveryMode,
    /// Told the broker's answer to each subscribe and publish (`BrokerAnswered`): the facade, which keeps what was
    /// refused. None for a client without one.
    pub answers: Option<ActorRef<TcpClientMessage>>,
}

/// TCP client actor
pub struct TcpClientActor;

impl TcpClientActor {
    fn current_trace_ctx() -> Option<WireTraceCtx> {
        // Use the helper method from WireTraceCtx itself
        WireTraceCtx::from_current_span()
    }

    #[tracing::instrument(name = "cassini.tcp_client.connect_with_backoff", skip(client_config))]
    async fn connect_with_backoff(
        addr: String,
        server_name: String,
        client_config: &Arc<ClientConfig>,
    ) -> Result<TlsStream<TcpStream>, ActorProcessingErr> {
        let connector = TlsConnector::from(Arc::clone(client_config));
        let server_name = ServerName::try_from(server_name)
            .map_err(|_| ActorProcessingErr::from("Invalid DNS name"))?;

        // Each try is bounded: with the broker gone its address answers nothing, and an
        // unbounded try would wait on the kernel's retransmits (see connect.rs).
        connect::with_retries(&connect::Policy::default(), |_| {
            let connector = connector.clone();
            let server_name = server_name.clone();
            let addr = addr.clone();
            async move {
                let tcp = TcpStream::connect(addr)
                    .await
                    .map_err(|e| format!("TCP connect failed: {e}"))?;
                // Convert to std::net::TcpStream to set socket options
                let std_stream = tcp
                    .into_std()
                    .map_err(|e| format!("Failed to convert to std stream: {e}"))?;

                // Find a dead broker in about a minute, whether the connection is idle or
                // has data in flight, instead of waiting on it forever.
                if let Err(e) = cassini_types::socket::enable_dead_peer_detection(&std_stream) {
                    warn!("could not enable dead-peer detection on the broker connection: {e}");
                }

                // Use socket2 to set buffer sizes
                let sock_ref = SockRef::from(&std_stream);
                sock_ref
                    .set_recv_buffer_size(100 * 1024 * 1024)
                    .map_err(|e| format!("Failed to set recv buffer size: {e}"))?;
                sock_ref
                    .set_send_buffer_size(100 * 1024 * 1024)
                    .map_err(|e| format!("Failed to set send buffer size: {e}"))?;

                // Convert back to tokio stream
                let tcp = tokio::net::TcpStream::from_std(std_stream)
                    .map_err(|e| format!("Failed to convert back to tokio stream: {e}"))?;

                connector
                    .connect(server_name, tcp)
                    .await
                    .map_err(|e| format!("TLS handshake failed: {e}"))
            }
        })
        .await
        .map_err(|last| {
            ActorProcessingErr::from(format!("Failed to connect after retries: {last}"))
        })
    }

    fn spawn_reader(
        &self,
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
    ) -> Result<(), ActorProcessingErr> {
        // Take ownership of the read half exactly once.
        let reader = state.reader.take().expect("Reader already taken");

        let delivery = state.delivery.clone();
        let answers = state.answers.clone();
        let broker_addr = state.bind_addr.clone();
        let reconnect = state.reconnect;

        let read_loop_span =
            info_span!("cassini.tcp_client.read_loop", broker_addr = %broker_addr,);

        let abort = tokio::spawn(
            async move {
                let mut msg_count = 0;
                let mut buf_reader = tokio::io::BufReader::new(reader);
                loop {
                    msg_count += 1;
                    trace!("TCP client read loop iteration {}", msg_count);
                    match buf_reader.read_u32().await {
                        Ok(incoming_msg_length) => {
                            if incoming_msg_length > 0 {
                                let mut buffer = vec![0; incoming_msg_length as usize];

                                trace!("Reading {incoming_msg_length} byte(s).");
                                if buf_reader.read_exact(&mut buffer).await.is_ok() {
                                    let _fg = trace_span!(
                                        "tcp_client.read_frame",
                                        len = incoming_msg_length,
                                        first_16 = ?&buffer[..buffer.len().min(16)],
                                    )
                                    .entered();

                                    trace!(
                                        len = incoming_msg_length,
                                        first_16 = ?&buffer[..buffer.len().min(16)],
                                        "Received frame"
                                    );

                                    match rkyv::from_bytes::<ClientMessage, Error>(&buffer) {
                                        Ok(message) => {
                                            TcpClientActor::process_client_message(
                                                myself.clone(),
                                                &delivery,
                                                answers.as_ref(),
                                                message,
                                            );
                                        }
                                        Err(e) => {
                                            error!(
                                                len = buffer.len(),
                                                first_8 = ?&buffer[..buffer.len().min(8)],
                                                "{e} Received raw frame"
                                            );
                                            continue;
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "TCP reader terminating");
                            let event = ClientEvent::TransportError {
                                reason: format!("{UNEXPECTED_DISCONNECT} {e}"),
                            };
                            TcpClientActor::dispatch_event(&delivery, event);
                            match reconnect {
                                // Instead of breaking, send Reconnect to self
                                ReconnectMode::InPlace => {
                                    let _ = myself.send_message(TcpClientMessage::Reconnect);
                                }
                                // The supervisor brings a new transport; this one is done.
                                ReconnectMode::LeaveToSupervisor => {
                                    myself.stop(Some(format!("{UNEXPECTED_DISCONNECT} {e}")));
                                }
                            }
                            break;
                        }
                    }
                }
            }
            .instrument(read_loop_span),
        )
        .abort_handle();

        state.abort_handle = Some(abort);
        Ok(())
    }

    /// Given an already-parsed, real `ClientMessage` off the wire,
    /// decides and executes what happens next -- extracted from
    /// spawn_reader's own, previously-tightly-interleaved read loop
    /// specifically so this decision logic is directly, genuinely
    /// unit-testable without needing a real socket or async runtime
    /// at all: nothing in this function actually awaits anything, it
    /// was simply nested inside spawn_reader's own, real, async read
    /// loop before this.
    fn process_client_message(
        myself: ActorRef<TcpClientMessage>,
        delivery: &DeliveryMode,
        answers: Option<&ActorRef<TcpClientMessage>>,
        message: ClientMessage,
    ) {
        let answer = |topic: &str, use_: facade::TopicUse, result: &Result<(), String>| {
            if let Some(answers) = answers
                && let Err(e) = answers.send_message(TcpClientMessage::BrokerAnswered {
                    topic: topic.to_string(),
                    use_,
                    result: result.clone(),
                })
            {
                warn!("the broker's answer on {topic} could not be passed on: {e}");
            }
        };
        match message {
            ClientMessage::RegistrationResponse { result, trace_ctx } => {
                let span = trace_span!("client.handle_registration_response");
                try_set_parent_wire(&span, trace_ctx);
                let _g = span.enter();

                match result {
                    Ok(registration_id) => {
                        info!("Registration succeeded: {registration_id}");
                        myself
                            .send_message(TcpClientMessage::RegistrationResponse(registration_id))
                            .expect("Could not forward message to {myself:?");
                    }
                    Err(e) => {
                        warn!("Failed to register session with the server. {e}");
                    }
                }
            }
            ClientMessage::PublishResponse {
                topic,
                payload,
                result,
                trace_ctx,
                published_by,
            } => {
                let span = trace_span!("client.handle_publish_response");
                if let Some(wire_ctx) = trace_ctx {
                    tracing::trace!(
                        "Received PublishResponse with trace_id: {:02x?}",
                        wire_ctx.trace_id
                    );
                }
                try_set_parent_wire(&span, trace_ctx);
                let _g = span.enter();

                if result.is_ok() {
                    let current_trace_ctx = WireTraceCtx::from_current_span();
                    let event = ClientEvent::MessagePublished {
                        topic: topic.clone(),
                        payload: payload.to_vec(),
                        trace_ctx: current_trace_ctx,
                        published_by: published_by.clone(),
                    };
                    TcpClientActor::dispatch_event(delivery, event);
                    trace!("TCP client dispatched MessagePublished for topic {}", topic);
                } else if let Err(reason) = result {
                    // The broker did not take our publish: the agent hears it, not only the log.
                    warn!("The broker refused a publish on {topic}: {reason}");
                    answer(&topic, facade::TopicUse::Publish, &Err(reason.clone()));
                    TcpClientActor::dispatch_event(
                        delivery,
                        ClientEvent::PublishRefused {
                            topic: topic.clone(),
                            reason: reason.clone(),
                        },
                    );
                }
            }
            ClientMessage::SubscribeAcknowledgment {
                topic,
                result,
                trace_ctx,
            } => {
                let span = trace_span!("client.handle_subscribe_ack");
                try_set_parent_wire(&span, trace_ctx);
                let _g = span.enter();

                answer(&topic, facade::TopicUse::Subscribe, &result);
                match result {
                    Ok(()) => {
                        info!("Subscribed to topic: {topic}");
                        TcpClientActor::dispatch_event(
                            delivery,
                            ClientEvent::ClientSubscribed {
                                topic: topic.clone(),
                            },
                        );
                    }
                    Err(reason) => {
                        warn!("The broker refused a subscription to {topic}: {reason}");
                        TcpClientActor::dispatch_event(
                            delivery,
                            ClientEvent::SubscribeRefused {
                                topic: topic.clone(),
                                reason: reason.clone(),
                            },
                        );
                    }
                }
            }

            ClientMessage::UnsubscribeAcknowledgment {
                topic,
                result,
                trace_ctx,
            } => {
                let span = trace_span!("client.handle_unsubscribe_ack");
                try_set_parent_wire(&span, trace_ctx);
                let _g = span.enter();

                if result.is_ok() {
                    info!("Successfully unsubscribed from topic: {topic}");
                } else {
                    warn!("Failed to unsubscribe from topic: {topic}");
                }
            }
            ClientMessage::ErrorMessage { error, trace_ctx } => {
                let span = trace_span!("client.handle_error_message").entered();
                try_set_parent_wire(&span, trace_ctx);
                let _g = span.enter();
                warn!("Received error from broker: {error}");

                // A session-mismatch response means the broker has no record of
                // our registration_id (e.g. it restarted/rejuvenated). Resuming
                // is impossible; force a fresh registration instead of retrying
                // the same rejected request forever.
                if error.contains("session mismatch") {
                    warn!("Session no longer valid on broker; re-registering fresh");
                    let _ = myself.send_message(TcpClientMessage::SessionInvalidated);
                }
            }
            ClientMessage::PublishRequestAck { topic, trace_ctx } => {
                let span = trace_span!("client.handle_publish_ack");
                try_set_parent_wire(&span, trace_ctx);
                let _g = span.enter();
                debug!("Received publish ack for topic: {topic}");

                let event = ClientEvent::PublishAcknowledged { topic };
                TcpClientActor::dispatch_event(delivery, event);
            }
            ClientMessage::ControlResponse {
                registration_id,
                result,
                trace_ctx,
            } => {
                let span = trace_span!("client.handle_control_response");
                try_set_parent_wire(&span, trace_ctx);
                let _g = span.enter();

                trace!(
                    "Received control response for registration: {registration_id}, result: {:?}",
                    result
                );

                let current_trace_ctx = WireTraceCtx::from_current_span();
                let event = ClientEvent::ControlResponse {
                    registration_id,
                    result,
                    trace_ctx: current_trace_ctx,
                };
                TcpClientActor::dispatch_event(delivery, event);
            }
            _ => {
                let _g = trace_span!("client.handle_unknown_message").entered();
                trace!("Received unexpected message type");
            }
        }
    }

    /// Send a payload to the broker over the write.
    pub async fn send_message(
        message: ClientMessage,
        state: &mut TcpClientState,
    ) -> Result<(), ActorProcessingErr> {
        // NOTE: Broker expects u32 length prefix, NOT u64
        match rkyv::to_bytes::<Error>(&message) {
            Ok(bytes) => {
                // Validate frame size fits in u32
                let len: u32 = bytes
                    .len()
                    .try_into()
                    .map_err(|_| ActorProcessingErr::from("Frame too large (>4GB)"))?;

                // Pre-allocate buffer with exact capacity (4 bytes + payload)
                let mut buffer = Vec::with_capacity(4 + bytes.len());

                // Write 4-byte BE length prefix (matching broker's read_u32)
                buffer.extend_from_slice(&len.to_be_bytes());

                //add message to buffer
                buffer.extend_from_slice(&bytes);

                //write message
                if let Some(arc_writer) = state.writer.as_ref() {
                    let mut writer = arc_writer.lock().await;

                    // Write entire buffer in one go
                    writer.write_all(&buffer).await?;

                    // Flush to ensure data is sent immediately (CRITICAL!)
                    writer.flush().await?;
                    Ok(())
                } else {
                    error!("No writer available in TcpClientState");
                    let reason = "No writer assigned to client!".to_string();
                    Err(ActorProcessingErr::from(reason))
                }
            }
            Err(e) => {
                warn!("Failed to serialize message. {e}");
                Err(ActorProcessingErr::from(e))
            }
        }
    }

    pub fn emit_transport_err_and_stop(
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
        reason: String,
    ) {
        let _g = warn_span!("tcp_client.transport_error", error = %reason).entered();
        Self::dispatch_event(
            &state.delivery,
            ClientEvent::TransportError {
                reason: reason.clone(),
            },
        );
        drop(_g);
        myself.stop(Some(reason));
    }

    /// The one, real, single place a `ClientEvent` is ever actually
    /// delivered out of this actor -- replaces four, separate,
    /// previously near-identical "send to the broadcast port, then
    /// conditionally also send to a direct handler" call sites (see
    /// this crate's own, prior history for the shape this replaced).
    ///
    /// `MessagePublished` specifically is the one, real variant
    /// `DeliveryMode::Router` treats differently from every other
    /// variant -- it's the only one that's genuinely "a message on a
    /// topic," and the only one converted into a real
    /// `polar_router::IncomingMessage` and routed by topic. Every
    /// other, real `ClientEvent` (connection-lifecycle events like
    /// `Registered`/`TransportError`, never genuinely topic-shaped at
    /// all) goes directly to `lifecycle` instead.
    fn dispatch_event(delivery: &DeliveryMode, event: ClientEvent) {
        match delivery {
            DeliveryMode::Broadcast(events_output) => {
                events_output.send(event);
            }
            DeliveryMode::Router { router, lifecycle } => match event {
                ClientEvent::MessagePublished {
                    topic,
                    payload,
                    trace_ctx,
                    published_by,
                } => {
                    if let Err(e) = router.send_message(polar_router::RouterMessage::Route(
                        polar_router::IncomingMessage {
                            topic,
                            payload,
                            published_by,
                            trace_ctx,
                        },
                    )) {
                        error!("Failed to route MessagePublished to RouterActor: {e}");
                    }
                }
                other => lifecycle(other),
            },
        }
    }

    /// The real, single, shared shape behind nine, separate,
    /// previously near-identical outbound handler functions (each
    /// built its own real ClientMessage envelope, sent it, and on
    /// error called emit_transport_err_and_stop -- differing only in
    /// which envelope variant and fields). Always returns Ok(()) --
    /// a real send failure is never propagated up as an error, only
    /// logged and emitted as a real, genuine ClientEvent::TransportError,
    /// matching every one of the original nine call sites' own,
    /// existing behavior exactly.
    async fn send_or_fail(
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
        envelope: ClientMessage,
    ) -> Result<(), ActorProcessingErr> {
        if let Err(e) = TcpClientActor::send_message(envelope, state).await {
            TcpClientActor::emit_transport_err_and_stop(myself, state, e.to_string());
        }
        Ok(())
    }

    // ===== Handler methods =====

    async fn handle_reconnected(
        &self,
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
        reader: Option<ReadHalf<TlsStream<TcpStream>>>,
        writer: Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>,
    ) -> Result<(), ActorProcessingErr> {
        state.reader = reader;
        state.writer = Some(writer);
        self.spawn_reader(myself.clone(), state)?;
        Ok(())
    }

    async fn handle_reconnect_registered(
        &self,
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
    ) -> Result<(), ActorProcessingErr> {
        info!("Attempting to reconnect...");
        state.writer = None;
        if let Some(handle) = state.abort_handle.take() {
            handle.abort();
        }

        let addr = state.bind_addr.clone();
        let server_name = state.server_name.clone();
        let client_config = state.client_config.clone();
        let myself_clone = myself.clone();
        tokio::spawn(async move {
            match TcpClientActor::connect_with_backoff(addr, server_name, &client_config).await {
                Ok(tls_stream) => {
                    let (reader, write_half) = split(tls_stream);
                    let writer = tokio::io::BufWriter::new(write_half);
                    let _ = myself_clone.send_message(TcpClientMessage::Reconnected {
                        reader: Some(reader),
                        writer: Arc::new(Mutex::new(writer)),
                    });
                    let _ = myself_clone.send_message(TcpClientMessage::Register);
                }
                Err(e) => {
                    error!("Reconnection failed: {}", e);
                    myself_clone.stop(Some("Reconnection failed".to_string()));
                }
            }
        });
        Ok(())
    }

    // Unregistered state handlers
    async fn handle_register_unregistered(
        &self,
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
    ) -> Result<(), ActorProcessingErr> {
        let envelope = ClientMessage::RegistrationRequest {
            registration_id: None,
            trace_ctx: Self::current_trace_ctx(),
        };
        TcpClientActor::send_or_fail(myself, state, envelope).await
    }

    /// The broker accepted a registration: this is the session from now on, and the agent is
    /// told, so it subscribes. Used whether the client was unregistered or was asking to resume
    /// (the broker may answer a resume with the same session, or, after a restart that lost it,
    /// a new one); in both the agent must be told, and subscribing again is harmless.
    async fn handle_registration_response_unregistered(
        &self,
        _myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
        registration_id: String,
    ) -> Result<(), ActorProcessingErr> {
        state.registration = RegistrationState::Registered {
            registration_id: registration_id.clone(),
        };
        let event = ClientEvent::Registered { registration_id };
        TcpClientActor::dispatch_event(&state.delivery, event);
        Ok(())
    }

    // Registered state handlers
    async fn handle_register_registered(
        &self,
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
        registration_id: &str,
    ) -> Result<(), ActorProcessingErr> {
        let envelope = ClientMessage::RegistrationRequest {
            registration_id: Some(registration_id.to_owned()),
            trace_ctx: Self::current_trace_ctx(),
        };
        TcpClientActor::send_or_fail(myself, state, envelope).await
    }

    async fn handle_get_registration_id(
        &self,
        _myself: ActorRef<TcpClientMessage>,
        _state: &mut TcpClientState,
        registration_id: &str,
        reply: RpcReplyPort<Option<String>>,
    ) -> Result<(), ActorProcessingErr> {
        reply.send(Some(registration_id.to_owned())).ok();
        Ok(())
    }

    async fn handle_publish(
        &self,
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
        registration_id: &str,
        topic: String,
        payload: Vec<u8>,
        trace_ctx: Option<WireTraceCtx>,
        share_identity: bool,
    ) -> Result<(), ActorProcessingErr> {
        let envelope = ClientMessage::PublishRequest {
            topic,
            payload: payload.into(),
            registration_id: registration_id.to_owned(),
            trace_ctx,
            share_identity,
        };
        TcpClientActor::send_or_fail(myself, state, envelope).await
    }

    async fn handle_subscribe(
        &self,
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
        registration_id: &str,
        topic: String,
        trace_ctx: Option<WireTraceCtx>,
    ) -> Result<(), ActorProcessingErr> {
        let envelope = ClientMessage::SubscribeRequest {
            registration_id: registration_id.to_owned(),
            topic,
            trace_ctx,
        };
        TcpClientActor::send_or_fail(myself, state, envelope).await
    }

    async fn handle_unsubscribe_request(
        &self,
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
        registration_id: &str,
        topic: String,
        trace_ctx: Option<WireTraceCtx>,
    ) -> Result<(), ActorProcessingErr> {
        let envelope = ClientMessage::UnsubscribeRequest {
            registration_id: registration_id.to_owned(),
            topic,
            trace_ctx,
        };
        TcpClientActor::send_or_fail(myself, state, envelope).await
    }

    async fn handle_disconnect(
        &self,
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
        registration_id: &str,
        trace_ctx: Option<WireTraceCtx>,
    ) -> Result<(), ActorProcessingErr> {
        info!("Received disconnect signal. Shutting down client.");
        debug!("Flushing pending writes before disconnect...");
        sleep(Duration::from_millis(100)).await;
        let envelope = ClientMessage::DisconnectRequest {
            registration_id: Some(registration_id.to_owned()),
            trace_ctx,
        };
        TcpClientActor::send_or_fail(myself.clone(), state, envelope).await?;
        if let Some(handle) = state.abort_handle.take() {
            handle.abort();
        }
        myself.stop(Some(CLIENT_DISCONNECTED.to_string()));
        Ok(())
    }

    async fn handle_control_request(
        &self,
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
        registration_id: &str,
        op: ControlOp,
        trace_ctx: Option<WireTraceCtx>,
    ) -> Result<(), ActorProcessingErr> {
        let envelope = ClientMessage::ControlRequest {
            registration_id: registration_id.to_owned(),
            op,
            trace_ctx,
        };
        TcpClientActor::send_or_fail(myself, state, envelope).await
    }

    async fn handle_list_sessions(
        &self,
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
        registration_id: &str,
        trace_ctx: Option<WireTraceCtx>,
    ) -> Result<(), ActorProcessingErr> {
        let envelope = ClientMessage::ControlRequest {
            registration_id: registration_id.to_owned(),
            op: ControlOp::ListSessions,
            trace_ctx,
        };
        TcpClientActor::send_or_fail(myself, state, envelope).await
    }

    async fn handle_list_topics(
        &self,
        myself: ActorRef<TcpClientMessage>,
        state: &mut TcpClientState,
        registration_id: &str,
        trace_ctx: Option<WireTraceCtx>,
    ) -> Result<(), ActorProcessingErr> {
        let envelope = ClientMessage::ControlRequest {
            registration_id: registration_id.to_owned(),
            op: ControlOp::ListTopics,
            trace_ctx,
        };
        TcpClientActor::send_or_fail(myself, state, envelope).await
    }

    // Fallback for unregistered state
    async fn handle_unregistered_fallback(
        &self,
        _myself: ActorRef<TcpClientMessage>,
        _state: &mut TcpClientState,
        msg: TcpClientMessage,
    ) -> Result<(), ActorProcessingErr> {
        warn!("Ignoring message {:?}, client is unregistered.", msg);
        Ok(())
    }

    // Fallback for registered state for messages not explicitly handled
    async fn handle_registered_fallback(
        &self,
        _myself: ActorRef<TcpClientMessage>,
        _state: &mut TcpClientState,
        _registration_id: &str,
        _msg: TcpClientMessage,
    ) -> Result<(), ActorProcessingErr> {
        // ignore other messages like Reconnect (already handled at top level)
        Ok(())
    }
}

#[async_trait]
impl Actor for TcpClientActor {
    type Msg = TcpClientMessage;
    type State = TcpClientState;
    type Arguments = TcpClientArgs;

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        args: TcpClientArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        // Keep the "init" span short-lived and add the key identifiers as fields.
        let init_span = info_span!(
            "cassini.tcp_client.init",
            broker_addr = %args.config.broker_endpoint,
            server_name = %args.config.server_name,
            has_registration_id = args.registration_id.is_some(),
        );
        let _g = init_span.enter();
        info!("Starting TCP Client");

        // --- 1. Initialize crypto provider once ---
        let _g = debug_span!("tls.install_crypto_provider").entered();
        match rustls::crypto::ring::default_provider().install_default() {
            Ok(_) => warn!("Default crypto provider installed (this should only happen once)"),
            Err(_) => debug!("Crypto provider already configured"),
        }
        drop(_g);

        // --- 2. Load CA certificate into root store ---
        let _g = debug_span!("tls.load_ca_cert").entered();
        let mut root_cert_store = rustls::RootCertStore::empty();
        let ca_path = &args.config.ca_certificate_path;

        let ca_cert = CertificateDer::from_pem_file(ca_path).map_err(|e| {
            ActorProcessingErr::from(anyhow::anyhow!(
                "Failed to read CA certificate from {ca_path}: {e}"
            ))
        })?;

        root_cert_store.add(ca_cert).map_err(|e| {
            ActorProcessingErr::from(anyhow::anyhow!(
                "Failed to add CA certificate to root store: {e}"
            ))
        })?;
        drop(_g);

        // --- 3. Build WebPKI verifier ---
        let _g = debug_span!("tls.build_verifier").entered();
        let verifier = WebPkiServerVerifier::builder(Arc::new(root_cert_store))
            .build()
            .map_err(|e| {
                ActorProcessingErr::from(anyhow::anyhow!("Failed to build WebPKI verifier: {e}"))
            })?;
        drop(_g);

        // --- 4. Load client cert and key ---
        let _g = debug_span!("tls.load_client_cert_key").entered();
        let mut certs = Vec::new();
        let cert_path = &args.config.client_certificate_path;
        let key_path = &args.config.client_key_path;

        let client_cert = CertificateDer::from_pem_file(cert_path).map_err(|e| {
            ActorProcessingErr::from(anyhow::anyhow!(
                "Failed to read client certificate from {cert_path}: {e}"
            ))
        })?;

        certs.push(client_cert);

        let private_key = PrivateKeyDer::from_pem_file(key_path).map_err(|e| {
            ActorProcessingErr::from(anyhow::anyhow!(
                "Failed to read client private key from {key_path}: {e}"
            ))
        })?;
        drop(_g);

        // --- 5. Build Rustls client config ---
        let _g = debug_span!("tls.build_client_config").entered();
        let client_config = rustls::ClientConfig::builder()
            .with_webpki_verifier(verifier)
            .with_client_auth_cert(certs, private_key)
            .map_err(|e| {
                ActorProcessingErr::from(anyhow::anyhow!(
                    "Failed to build Rustls client config: {e}"
                ))
            })?;
        drop(_g);

        // --- 6. Determine registration state ---
        let registration = match args.registration_id {
            Some(id) => RegistrationState::Registered {
                registration_id: id,
            },
            None => RegistrationState::Unregistered,
        };

        // --- 7. Construct final state ---
        // init_span ends when this function returns (no long-lived spawned tasks here)
        let state = TcpClientState {
            reconnect: args.config.reconnect,
            bind_addr: args.config.broker_endpoint,
            server_name: args.config.server_name,
            reader: None,
            writer: None,
            registration,
            client_config: Arc::new(client_config),
            delivery: args.delivery,
            abort_handle: None,
            answers: args.answers,
        };

        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let addr = state.bind_addr.clone();
        let connect_span = info_span!(
            "cassini.tcp_client.connect",
            broker_addr = %addr,
            server_name = %state.server_name,
        );
        let _g = connect_span.enter();
        info!("Connecting to {addr}");

        let tls_stream = TcpClientActor::connect_with_backoff(
            addr.clone(),
            state.server_name.clone(),
            &state.client_config,
        )
        .await?;

        info!("TLS connection established");

        let (reader, write_half) = split(tls_stream);
        let writer = tokio::io::BufWriter::new(write_half);

        state.reader = Some(reader);
        state.writer = Some(Arc::new(Mutex::new(writer)));

        info!("Successfully connected to {addr}. Listening for messages.");

        // Spawn the long-running reader loop under its own span (spawn_reader should instrument the task).
        self.spawn_reader(myself.clone(), state)?;

        // Kick off registration *after* connection is guaranteed
        info!("Requesting registration");
        myself
            .send_message(TcpClientMessage::Register)
            .map_err(ActorProcessingErr::from)?;

        Ok(())
    }

    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Successfully stopped {myself:?}");
        if let Some(handle) = state.abort_handle.take() {
            handle.abort();
        }
        Ok(())
    }

    // The message is recorded by its kind, never by Debug: Debug writes a publish's payload out byte by byte, and this
    // span is at INFO, so every publish of every agent paid about 27 ms a MiB for it (stage 5a's load harness).
    #[tracing::instrument(
        name = "cassini.tcp_client.handle",
        skip(self, myself, state, message),
        fields(registration = ?state.registration, msg = message.kind()),
    )]
    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match (&mut state.registration, message) {
            // ---- Reconnected (always handled) ----
            (_, TcpClientMessage::Reconnected { reader, writer }) => {
                self.handle_reconnected(myself, state, reader, writer)
                    .await?
            }

            // ---- SessionInvalidated (always handled) ----
            (_, TcpClientMessage::SessionInvalidated) => {
                warn!("Resetting registration state and re-registering");
                state.registration = RegistrationState::Unregistered;
                self.handle_register_unregistered(myself, state).await?
            }

            // ---- Reconnect (only when registered) ----
            (RegistrationState::Registered { .. }, TcpClientMessage::Reconnect) => {
                self.handle_reconnect_registered(myself, state).await?
            }

            // ---- Unregistered state ----
            (RegistrationState::Unregistered, TcpClientMessage::Register) => {
                self.handle_register_unregistered(myself, state).await?
            }
            (
                RegistrationState::Unregistered,
                TcpClientMessage::RegistrationResponse(registration_id),
            ) => {
                self.handle_registration_response_unregistered(myself, state, registration_id)
                    .await?
            }
            (RegistrationState::Unregistered, msg) => {
                self.handle_unregistered_fallback(myself, state, msg)
                    .await?
            }

            // ---- A registration answer while already registered ----
            // The client asked to resume a session. Whatever the broker answers, that is the
            // session now, and the agent is told. Ignoring it left the agent running and deaf
            // after a broker restart, because nothing resubscribed.
            (
                RegistrationState::Registered { .. },
                TcpClientMessage::RegistrationResponse(registration_id),
            ) => {
                self.handle_registration_response_unregistered(myself, state, registration_id)
                    .await?
            }

            // ---- Registered state ----
            (RegistrationState::Registered { registration_id }, msg) => {
                // Clone registration_id to release the borrow on state.registration
                let reg_id = registration_id.clone();
                match msg {
                    TcpClientMessage::Register => {
                        self.handle_register_registered(myself, state, &reg_id)
                            .await?;
                    }
                    TcpClientMessage::GetRegistrationId(reply) => {
                        self.handle_get_registration_id(myself, state, &reg_id, reply)
                            .await?;
                    }
                    TcpClientMessage::Publish {
                        topic,
                        payload,
                        trace_ctx,
                        share_identity,
                    } => {
                        self.handle_publish(
                            myself,
                            state,
                            &reg_id,
                            topic,
                            payload,
                            trace_ctx,
                            share_identity,
                        )
                        .await?;
                    }
                    TcpClientMessage::Subscribe { topic, trace_ctx } => {
                        self.handle_subscribe(myself, state, &reg_id, topic, trace_ctx)
                            .await?;
                    }
                    TcpClientMessage::UnsubscribeRequest { topic, trace_ctx } => {
                        self.handle_unsubscribe_request(myself, state, &reg_id, topic, trace_ctx)
                            .await?;
                    }
                    TcpClientMessage::Disconnect { trace_ctx } => {
                        self.handle_disconnect(myself, state, &reg_id, trace_ctx)
                            .await?;
                    }
                    TcpClientMessage::ErrorMessage(error) => {
                        warn!("Received error from broker: {error}");
                    }
                    TcpClientMessage::ControlRequest { op, trace_ctx } => {
                        self.handle_control_request(myself, state, &reg_id, op, trace_ctx)
                            .await?;
                    }
                    TcpClientMessage::ListSessions { trace_ctx } => {
                        self.handle_list_sessions(myself, state, &reg_id, trace_ctx)
                            .await?;
                    }
                    TcpClientMessage::ListTopics { trace_ctx } => {
                        self.handle_list_topics(myself, state, &reg_id, trace_ctx)
                            .await?;
                    }
                    // Ignore other messages like Reconnect (already handled at top level)
                    _ => {
                        self.handle_registered_fallback(myself, state, &reg_id, msg)
                            .await?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod process_client_message_tests {
    /// A message is named by its kind, and the kind does not hold the payload, however large it is.
    #[test]
    fn a_message_is_named_by_its_kind_not_its_payload() {
        let publish = TcpClientMessage::publish("t".to_string(), vec![7u8; 1 << 20], None);
        assert_eq!(publish.kind(), "Publish");
        assert_eq!(TcpClientMessage::Register.kind(), "Register");
        assert_eq!(
            TcpClientMessage::SessionInvalidated.kind(),
            "SessionInvalidated"
        );
    }

    use super::*;
    use std::sync::{Arc, Mutex};

    /// A minimal, real, fake stand-in for TcpClientActor itself --
    /// genuinely receives TcpClientMessage, just records what it got
    /// rather than acting on it. Lets these tests assert on the real,
    /// specific messages process_client_message forwards to `myself`
    /// (e.g. SessionInvalidated), without needing a real,
    /// full TcpClientActor with an actual TLS connection at all.
    struct FakeTcpClient {
        received: Arc<Mutex<Vec<TcpClientMessage>>>,
    }

    #[async_trait]
    impl Actor for FakeTcpClient {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            self.received.lock().unwrap().push(message);
            Ok(())
        }
    }

    async fn spawn_fake_tcp_client() -> (
        ActorRef<TcpClientMessage>,
        Arc<Mutex<Vec<TcpClientMessage>>>,
    ) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (actor, _) = Actor::spawn(
            None,
            FakeTcpClient {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();
        (actor, received)
    }

    async fn broadcast_delivery() -> (DeliveryMode, Arc<Mutex<Vec<ClientEvent>>>) {
        let captured: Arc<Mutex<Vec<ClientEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_for_closure = captured.clone();
        let events_output = Arc::new(OutputPort::default());

        // A real, minimal subscriber actor -- reused here purely as
        // OutputPort's own, required subscriber target, not itself
        // under test.
        struct Sink;
        #[async_trait]
        impl Actor for Sink {
            type Msg = ClientEvent;
            type State = ();
            type Arguments = ();
            async fn pre_start(
                &self,
                _myself: ActorRef<Self::Msg>,
                _args: (),
            ) -> Result<Self::State, ActorProcessingErr> {
                Ok(())
            }
            async fn handle(
                &self,
                _myself: ActorRef<Self::Msg>,
                _message: Self::Msg,
                _state: &mut Self::State,
            ) -> Result<(), ActorProcessingErr> {
                Ok(())
            }
        }

        // The real subscriber closure genuinely runs synchronously,
        // inline, on send() -- captured directly here rather than via
        // a second actor hop, since these tests only care about the
        // real content dispatch_event hands to events_output, not
        // about a second actor's own, separate mailbox timing.
        let (sink, _) = Actor::spawn(None, Sink, ()).await.unwrap();
        events_output.subscribe(sink, move |event: ClientEvent| {
            captured_for_closure.lock().unwrap().push(event.clone());
            Some(event)
        });

        (DeliveryMode::Broadcast(events_output), captured)
    }

    #[tokio::test]
    async fn a_successful_publish_response_dispatches_message_published() {
        let (myself, _received) = spawn_fake_tcp_client().await;
        let (delivery, captured) = broadcast_delivery().await;

        TcpClientActor::process_client_message(
            myself,
            &delivery,
            None,
            ClientMessage::PublishResponse {
                topic: "a-real-topic".to_string(),
                payload: b"real payload".to_vec().into(),
                result: Ok(()),
                trace_ctx: None,
                published_by: Some("some-agent".to_string()),
            },
        );

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let events = captured.lock().unwrap().clone();
        assert_eq!(events.len(), 1, "expected exactly one dispatched event");
        match &events[0] {
            ClientEvent::MessagePublished {
                topic,
                payload,
                published_by,
                ..
            } => {
                assert_eq!(topic, "a-real-topic");
                assert_eq!(payload, b"real payload");
                assert_eq!(published_by, &Some("some-agent".to_string()));
            }
            other => panic!("expected MessagePublished, got {other:?}"),
        }
    }

    /// The events the client dispatches for one message from the broker.
    async fn dispatched_for(message: ClientMessage) -> Vec<ClientEvent> {
        let (myself, _received) = spawn_fake_tcp_client().await;
        let (delivery, captured) = broadcast_delivery().await;
        TcpClientActor::process_client_message(myself, &delivery, None, message);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        captured.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn a_failed_publish_response_is_a_refusal_never_a_delivered_message() {
        let events = dispatched_for(ClientMessage::PublishResponse {
            topic: "a-real-topic".to_string(),
            payload: b"real payload".to_vec().into(),
            result: Err("broker rejected it".to_string()),
            trace_ctx: None,
            published_by: None,
        })
        .await;
        assert!(
            matches!(events.as_slice(), [ClientEvent::PublishRefused { topic, reason }]
                if topic == "a-real-topic" && reason == "broker rejected it"),
            "a failed publish is a PublishRefused, never a MessagePublished: {events:?}"
        );
    }

    #[tokio::test]
    async fn a_subscription_the_broker_took_is_acknowledged_to_the_agent() {
        let events = dispatched_for(ClientMessage::SubscribeAcknowledgment {
            topic: "news".to_string(),
            result: Ok(()),
            trace_ctx: None,
        })
        .await;
        assert!(
            matches!(events.as_slice(), [ClientEvent::ClientSubscribed { topic }] if topic == "news"),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn a_subscription_the_broker_refused_is_a_refusal_the_agent_sees() {
        let events = dispatched_for(ClientMessage::SubscribeAcknowledgment {
            topic: "news".to_string(),
            result: Err("refused: not declared".to_string()),
            trace_ctx: None,
        })
        .await;
        assert!(
            matches!(events.as_slice(), [ClientEvent::SubscribeRefused { topic, reason }]
                if topic == "news" && reason == "refused: not declared"),
            "{events:?}"
        );
    }

    /// The broker's answers to this client's subscribes and publishes, as whoever is told of them (the facade) hears
    /// them, for each message from the broker.
    async fn answered_for(
        messages: Vec<ClientMessage>,
    ) -> Vec<(String, crate::facade::TopicUse, Result<(), String>)> {
        let (myself, _) = spawn_fake_tcp_client().await;
        let (answers, told) = spawn_fake_tcp_client().await;
        let (delivery, _captured) = broadcast_delivery().await;
        for message in messages {
            TcpClientActor::process_client_message(
                myself.clone(),
                &delivery,
                Some(&answers),
                message,
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        told.lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                TcpClientMessage::BrokerAnswered {
                    topic,
                    use_,
                    result,
                } => Some((topic.clone(), *use_, result.clone())),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn the_broker_s_answers_to_subscribes_and_publishes_are_told_and_a_delivery_is_not_one() {
        use crate::facade::TopicUse;
        let answered = answered_for(vec![
            ClientMessage::SubscribeAcknowledgment {
                topic: "news".to_string(),
                result: Ok(()),
                trace_ctx: None,
            },
            ClientMessage::SubscribeAcknowledgment {
                topic: "weather".to_string(),
                result: Err("the broker refused: no".to_string()),
                trace_ctx: None,
            },
            ClientMessage::PublishResponse {
                topic: "sport".to_string(),
                payload: Vec::new().into(),
                result: Err("the broker refused: no".to_string()),
                trace_ctx: None,
                published_by: None,
            },
            // Someone else's message, delivered: not an answer to anything this client asked.
            ClientMessage::PublishResponse {
                topic: "news".to_string(),
                payload: b"x".to_vec().into(),
                result: Ok(()),
                trace_ctx: None,
                published_by: None,
            },
        ])
        .await;
        assert_eq!(
            answered,
            [
                ("news".to_string(), TopicUse::Subscribe, Ok(())),
                (
                    "weather".to_string(),
                    TopicUse::Subscribe,
                    Err("the broker refused: no".to_string())
                ),
                (
                    "sport".to_string(),
                    TopicUse::Publish,
                    Err("the broker refused: no".to_string())
                ),
            ]
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(24))]

        /// For any topic and reason: a refusal reaches the agent as exactly one refusal event carrying both, and a
        /// refusal is never mistaken for a delivery or a subscription taken.
        #[test]
        fn a_refusal_is_exactly_one_refusal_event_carrying_the_topic_and_reason(
            topic in "[a-z][a-z0-9.:-]{0,40}",
            reason in "[ -~]{0,80}",
            subscribe in proptest::bool::ANY,
        ) {
            let message = if subscribe {
                ClientMessage::SubscribeAcknowledgment {
                    topic: topic.clone(),
                    result: Err(reason.clone()),
                    trace_ctx: None,
                }
            } else {
                ClientMessage::PublishResponse {
                    topic: topic.clone(),
                    payload: Vec::new().into(),
                    result: Err(reason.clone()),
                    trace_ctx: None,
                    published_by: None,
                }
            };
            let events = tokio::runtime::Runtime::new().unwrap().block_on(dispatched_for(message));
            proptest::prop_assert_eq!(events.len(), 1, "{:?}", events);
            match &events[0] {
                ClientEvent::SubscribeRefused { topic: t, reason: r } if subscribe => {
                    proptest::prop_assert_eq!(t, &topic);
                    proptest::prop_assert_eq!(r, &reason);
                }
                ClientEvent::PublishRefused { topic: t, reason: r } if !subscribe => {
                    proptest::prop_assert_eq!(t, &topic);
                    proptest::prop_assert_eq!(r, &reason);
                }
                other => proptest::prop_assert!(false, "not the refusal expected: {:?}", other),
            }
        }
    }

    #[tokio::test]
    async fn a_session_mismatch_error_message_triggers_session_invalidated() {
        let (myself, received) = spawn_fake_tcp_client().await;
        let (delivery, _captured) = broadcast_delivery().await;

        TcpClientActor::process_client_message(
            myself,
            &delivery,
            None,
            ClientMessage::ErrorMessage {
                error: "session mismatch: unknown registration".to_string(),
                trace_ctx: None,
            },
        );

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let messages = received.lock().unwrap();
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, TcpClientMessage::SessionInvalidated)),
            "expected SessionInvalidated to have genuinely been forwarded to myself, got {messages:?}"
        );
    }

    #[tokio::test]
    async fn an_ordinary_error_message_never_triggers_session_invalidated() {
        let (myself, received) = spawn_fake_tcp_client().await;
        let (delivery, _captured) = broadcast_delivery().await;

        TcpClientActor::process_client_message(
            myself,
            &delivery,
            None,
            ClientMessage::ErrorMessage {
                error: "some other, unrelated broker error".to_string(),
                trace_ctx: None,
            },
        );

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let messages = received.lock().unwrap();
        assert!(
            !messages
                .iter()
                .any(|m| matches!(m, TcpClientMessage::SessionInvalidated)),
            "an ordinary error must never trigger a real re-registration"
        );
    }
}

#[cfg(test)]
mod registration_tests {
    use super::*;
    use std::sync::Mutex;

    async fn router() -> ActorRef<polar_router::RouterMessage> {
        Actor::spawn(None, polar_router::RouterActor, ())
            .await
            .unwrap()
            .0
    }

    /// A transport state with no connection, whose lifecycle events land in the returned list.
    async fn state(
        registration: RegistrationState,
    ) -> (TcpClientState, Arc<Mutex<Vec<ClientEvent>>>) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let state = TcpClientState {
            reconnect: ReconnectMode::default(),
            bind_addr: "broker:1".into(),
            server_name: "broker".into(),
            writer: None,
            reader: None,
            registration,
            client_config: Arc::new(
                rustls::ClientConfig::builder()
                    .with_root_certificates(rustls::RootCertStore::empty())
                    .with_no_client_auth(),
            ),
            delivery: DeliveryMode::Router {
                router: router().await,
                lifecycle: Arc::new(move |event| captured.lock().unwrap().push(event)),
            },
            abort_handle: None,
            answers: None,
        };
        (state, events)
    }

    async fn myself() -> ActorRef<TcpClientMessage> {
        struct Idle;
        #[async_trait]
        impl Actor for Idle {
            type Msg = TcpClientMessage;
            type State = ();
            type Arguments = ();
            async fn pre_start(
                &self,
                _myself: ActorRef<Self::Msg>,
                _args: (),
            ) -> Result<Self::State, ActorProcessingErr> {
                Ok(())
            }
        }
        Actor::spawn(None, Idle, ()).await.unwrap().0
    }

    fn registered_ids(events: &Arc<Mutex<Vec<ClientEvent>>>) -> Vec<String> {
        events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                ClientEvent::Registered { registration_id } => Some(registration_id.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_first_registration_is_reported() {
        let (mut state, events) = state(RegistrationState::Unregistered).await;
        TcpClientActor
            .handle(
                myself().await,
                TcpClientMessage::RegistrationResponse("first".into()),
                &mut state,
            )
            .await
            .unwrap();
        assert!(matches!(
            &state.registration,
            RegistrationState::Registered { registration_id } if registration_id == "first"
        ));
        assert_eq!(registered_ids(&events), ["first"]);
    }

    #[tokio::test]
    async fn a_new_session_given_to_a_client_that_asked_to_resume_is_reported() {
        // After a broker restart the broker has no record of the session the client asks to
        // resume, and answers with a new one. The client used to ignore that, so nothing
        // resubscribed and the agent was up and deaf.
        let (mut state, events) = state(RegistrationState::Registered {
            registration_id: "old".into(),
        })
        .await;
        TcpClientActor
            .handle(
                myself().await,
                TcpClientMessage::RegistrationResponse("new".into()),
                &mut state,
            )
            .await
            .unwrap();
        assert!(matches!(
            &state.registration,
            RegistrationState::Registered { registration_id } if registration_id == "new"
        ));
        assert_eq!(registered_ids(&events), ["new"]);
    }

    #[tokio::test]
    async fn a_resumed_session_is_reported_too_so_the_agent_resubscribes_harmlessly() {
        let (mut state, events) = state(RegistrationState::Registered {
            registration_id: "same".into(),
        })
        .await;
        TcpClientActor
            .handle(
                myself().await,
                TcpClientMessage::RegistrationResponse("same".into()),
                &mut state,
            )
            .await
            .unwrap();
        assert_eq!(registered_ids(&events), ["same"]);
    }
}
