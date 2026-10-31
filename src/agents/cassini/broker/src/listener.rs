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

use crate::UNEXPECTED_MESSAGE_STR;
use crate::authority::{BrokerAuthority, DecisionCache};
use crate::budget::{Bounds, Budget, Charge, SMALL_FRAME, Window, WindowCharge};
use crate::{
    PUBLISH_REQ_FAILED_TXT, REGISTRATION_REQ_FAILED_TXT, SESSION_MISSING_REASON_STR,
    SESSION_NOT_FOUND_TXT, SUBSCRIBE_REQUEST_FAILED_TXT,
};
use async_trait::async_trait;
use cassini_types::{
    ArchivedClientMessage, BrokerMessage, ClientMessage, ControlError, ControlOp, ControlResult,
    DisconnectReason, Hold, MAX_FRAME_BYTES, MessageId, Payload, ShutdownPhase, WireTraceCtx,
};
use polar_agent_builder::{Access, Floor};
use ractor::{
    Actor, ActorProcessingErr, ActorRef, SupervisionEvent, registry::where_is, rpc::CallResult,
};
use rkyv::{
    deserialize,
    rancor::{self, Error, Source},
};
use rustls::{
    RootCertStore, ServerConfig,
    pki_types::{CertificateDer, pem::PemObject},
    server::WebPkiClientVerifier,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::{
    io::{
        AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf,
        split,
    },
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task::JoinHandle,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tracing::{
    Instrument, debug, debug_span, error, info, info_span, instrument, trace, trace_span, warn,
};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use cassini_tracing::try_set_parent_otel;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

static ZERO_LEN_LAST_LOG_MS: AtomicU64 = AtomicU64::new(0);
static ZERO_LEN_COUNT: AtomicU64 = AtomicU64::new(0);

/// A connection's write half, and what ends it from outside: a write that waits on a client that does not read, and
/// the reader, both give up when it is cut. Its session holds it as its `ConnectionControl`, given by the Listener
/// (`BrokerMessage::ConnectionOpened`), and its reader tells the session what it reads and when it waits.
pub struct ConnWriter {
    half: Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>,
    cut: CancellationToken,
    why: std::sync::Mutex<Option<String>>,
    /// The connection's client id, as its session knows it.
    client_id: String,
    /// The session registered on this connection, once it is: its reader's facts go to it.
    session: std::sync::Mutex<Option<ActorRef<BrokerMessage>>>,
    /// When its reader last told its session of an acknowledgement, in milliseconds since the epoch (at most a few a
    /// second are told).
    told_progress_ms: AtomicU64,
    /// Until when its reader refuses for now a publish it cannot admit at once, instead of waiting for it, in
    /// milliseconds since the epoch (second review, finding A).
    refuse_until_ms: AtomicU64,
    /// Wakes its reader, waiting for an admission, to refuse it.
    refuse: tokio::sync::Notify,
    /// How many frames have been written to it: each write is numbered, from one, as its client counts what it reads.
    frames: AtomicU64,
}

impl std::fmt::Debug for ConnWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnWriter")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// How often a reader tells its session it read an acknowledgement, at most.
const PROGRESS_TOLD_EVERY: Duration = Duration::from_millis(100);

/// A reader waiting for an admission: its session is told when it starts and stops waiting.
struct Admitting<'a>(&'a ConnWriter);

impl<'a> Admitting<'a> {
    fn new(writer: &'a ConnWriter) -> Self {
        writer.tell_session(BrokerMessage::ConnectionWaiting {
            client_id: writer.client_id.clone(),
            waiting: true,
        });
        Admitting(writer)
    }
}

impl Drop for Admitting<'_> {
    fn drop(&mut self) {
        self.0.tell_session(BrokerMessage::ConnectionWaiting {
            client_id: self.0.client_id.clone(),
            waiting: false,
        });
    }
}

type SharedWriter = Arc<ConnWriter>;

impl ConnWriter {
    fn new(client_id: String, half: BufWriter<WriteHalf<TlsStream<TcpStream>>>) -> SharedWriter {
        Arc::new(ConnWriter {
            half: Mutex::new(half),
            cut: CancellationToken::new(),
            why: std::sync::Mutex::new(None),
            client_id,
            session: std::sync::Mutex::new(None),
            told_progress_ms: AtomicU64::new(0),
            refuse_until_ms: AtomicU64::new(0),
            refuse: tokio::sync::Notify::new(),
            frames: AtomicU64::new(0),
        })
    }

    /// Tells its session, if it has one yet.
    fn tell_session(&self, message: BrokerMessage) {
        if let Some(session) = self.session.lock().unwrap().as_ref() {
            let _ = session.send_message(message);
        }
    }

    /// Its reader read an acknowledgement: its session is told, at most every `PROGRESS_TOLD_EVERY`.
    fn progressed(&self) {
        let now = now_ms();
        let told = self.told_progress_ms.load(Ordering::SeqCst);
        if now.saturating_sub(told) >= PROGRESS_TOLD_EVERY.as_millis() as u64
            && self
                .told_progress_ms
                .compare_exchange(told, now, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            self.tell_session(BrokerMessage::ConnectionProgress {
                client_id: self.client_id.clone(),
            });
        }
    }

    /// Whether its reader refuses for now what it cannot admit at once.
    fn refusing(&self) -> bool {
        now_ms() < self.refuse_until_ms.load(Ordering::SeqCst)
    }

    fn why(&self) -> String {
        self.why
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "the connection was ended".to_string())
    }
}

/// What a session holds of its connection: weakly, so the connection closes when its Listener ends, whoever holds this.
#[derive(Debug)]
struct ConnHandle(std::sync::Weak<ConnWriter>);

impl cassini_types::ConnectionControl for ConnHandle {
    /// Ends the connection, saying `why`: its writes and its reader give up, and its Listener ends, as on a lost
    /// connection (its session is kept for the retention window).
    fn cut(&self, why: &str) {
        if let Some(writer) = self.0.upgrade() {
            *writer.why.lock().unwrap() = Some(why.to_string());
            writer.cut.cancel();
        }
    }

    /// Its reader refuses for now, for `how_long`, each publish it cannot admit at once, and stops waiting for the one
    /// it waits for: it reads on, so the acknowledgements behind them are read. For a connection whose session holds
    /// part of an exhausted budget while its reader waits for room: the room may be its own acknowledgements' to make.
    fn refuse_for(&self, how_long: Duration) {
        if let Some(writer) = self.0.upgrade() {
            let until = now_ms().saturating_add(how_long.as_millis() as u64);
            writer.refuse_until_ms.store(until, Ordering::SeqCst);
            writer.refuse.notify_one();
        }
    }
}

/// Takes a frame's bytes from its connection's window and from the budget, waiting for each; `None` if the connection
/// is cut meanwhile. While this waits its reader reads nothing, and TCP pushes back on the client.
async fn admit(
    window: &Arc<Window>,
    budget: &Budget,
    len: usize,
    cut: &CancellationToken,
) -> Option<(WindowCharge, Charge)> {
    let credit = match window.try_take(len) {
        Some(credit) => credit,
        None => {
            debug!(len, used = window.used(), "the reader waits for its window");
            tokio::select! {
                credit = window.take(len) => credit,
                _ = cut.cancelled() => return None,
            }
        }
    };
    let charge = match budget.try_charge(len) {
        Some(charge) => charge,
        None => {
            debug!(len, "the reader waits for the memory budget");
            tokio::select! {
                charge = budget.charge(len) => charge,
                _ = cut.cancelled() => return None,
            }
        }
    };
    Some((credit, charge))
}

/// Why a connection's reader stops.
enum Gone {
    /// The connection was cut from outside; its writer says why.
    Cut,
    /// The client closed it.
    Closed,
    /// A read failed, or the client sent what ends the connection.
    Failed(String),
}

impl Gone {
    fn reason(self, writer: &ConnWriter) -> DisconnectReason {
        match self {
            Gone::Cut => DisconnectReason::TransportError(writer.why()),
            Gone::Closed => DisconnectReason::RemoteClosed,
            Gone::Failed(why) => DisconnectReason::TransportError(why),
        }
    }
}

/// A frame read while a publish waited for its admission, to go on with once it is admitted.
enum Next {
    /// A frame too large to read before it is admitted: only its length was read.
    Header(usize),
    /// A frame read and decoded, and its length.
    Frame(usize, ClientMessage),
}

/// Reads a frame's length; 0 is a keepalive. A length over the frame limit ends the connection before anything is
/// allocated for it.
async fn read_header<R: AsyncRead + Unpin>(
    reader: &mut R,
    cut: &CancellationToken,
    client_id: &str,
    peer_identity: &Option<String>,
) -> Result<usize, Gone> {
    // Length header is u32 BE.
    let header = tokio::select! {
        header = reader.read_u32() => header,
        _ = cut.cancelled() => return Err(Gone::Cut),
    };
    let len = match header {
        Ok(v) => v as usize,
        // This is the normal exit path on remote close too.
        Err(e) => {
            return Err(match e.kind() {
                std::io::ErrorKind::UnexpectedEof => Gone::Closed,
                _ => Gone::Failed(e.to_string()),
            });
        }
    };
    if len == 0 {
        // Treat as keepalive/no-op and avoid log spam.
        let n = ZERO_LEN_COUNT.fetch_add(1, Ordering::Relaxed) + 1;

        // Log at most once every 5 seconds with a counter.
        let t = now_ms();
        let last = ZERO_LEN_LAST_LOG_MS.load(Ordering::Relaxed);
        if t.saturating_sub(last) > 5000 {
            ZERO_LEN_LAST_LOG_MS.store(t, Ordering::Relaxed);
            debug!(client_id = %client_id, zero_len_seen = n, "Zero-length frames received (suppressed)");
        }
        return Ok(0);
    }
    // guard against oversized, potentially insane message size
    if len > MAX_FRAME_BYTES {
        warn!(
            client_id = %client_id,
            identity = ?peer_identity,
            len,
            max = MAX_FRAME_BYTES,
            "Oversized frame; disconnecting before allocating"
        );
        return Err(Gone::Failed(format!(
            "frame length {len} exceeds max {MAX_FRAME_BYTES} bytes"
        )));
    }
    Ok(len)
}

/// Reads a frame's `len` bytes and decodes them; `None` if they do not decode (said in the log).
async fn read_body<R: AsyncRead + Unpin>(
    reader: &mut R,
    len: usize,
    cut: &CancellationToken,
    span: &tracing::Span,
) -> Result<Option<ClientMessage>, Gone> {
    let mut buffer = vec![0u8; len];
    let body = tokio::select! {
        body = reader.read_exact(&mut buffer).instrument(span.clone()) => body,
        _ = cut.cancelled() => return Err(Gone::Cut),
    };
    if let Err(e) = body {
        warn!(error = %e, "Failed reading frame body");
        return Err(Gone::Failed(e.to_string()));
    }
    let _g = span.enter();
    Ok(
        match rkyv::access::<ArchivedClientMessage, Error>(&buffer[..]) {
            Ok(archived) => match deserialize::<ClientMessage, Error>(archived) {
                Ok(deserialized) => {
                    trace!(msg = ?deserialized, "Decoded client message");
                    Some(deserialized)
                }
                Err(e) => {
                    warn!(error = ?e, "Failed to deserialize archived message");
                    None
                }
            },
            Err(e) => {
                warn!(error = ?e, "Failed to parse archived message");
                None
            }
        },
    )
}

/// What a frame's admission came to.
enum Admission {
    Admitted((WindowCharge, Charge)),
    /// Refused for now: its connection's reader refuses what it cannot admit at once (`ConnWriter::refuse_for`).
    Refused,
    /// The connection was cut while it waited.
    Cut,
}

/// What a publish refused for now is told (`PublishRefusal::Unavailable`; its client keeps it and sends it again).
pub const REFUSED_FOR_NOW: &str = "the broker's memory budget is exhausted, and this connection's acknowledgements \
    wait behind its publishes: refused for now, so they are read";

/// The one frame body read while its frame is refused (`admit_or_refuse`): its connection must read past it, and
/// what it reads is let go at once. One at a time, so the budget is passed by at most one frame, and only while
/// connections refuse.
static REFUSED_BODY: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// Takes a frame's bytes as [`admit`] does, unless its connection's reader is told to refuse for now: then a frame it
/// cannot admit at once is refused, and so is the one it waits for when it is told. While it waits it is marked as
/// waiting (`ConnWriter::is_admitting`).
async fn admit_or_refuse(
    window: &Arc<Window>,
    budget: &Budget,
    len: usize,
    cut: &CancellationToken,
    writer: &ConnWriter,
) -> Admission {
    if let Some(credit) = window.try_take(len)
        && let Some(charge) = budget.try_charge(len)
    {
        return Admission::Admitted((credit, charge));
    }
    if writer.refusing() {
        return Admission::Refused;
    }
    let _admitting = Admitting::new(writer);
    let mut admission = std::pin::pin!(admit(window, budget, len, cut));
    loop {
        tokio::select! {
            admission = &mut admission => {
                return match admission {
                    Some(admission) => Admission::Admitted(admission),
                    None => Admission::Cut,
                };
            }
            _ = writer.refuse.notified() => {
                if writer.refusing() {
                    return Admission::Refused;
                }
            }
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Extracts the connecting client's own DNS-SAN identity from their
/// verified mTLS certificate -- called once, right after the TLS
/// handshake completes, before the stream is split into separate
/// read/write halves (peer_certificates() is only reachable through
/// the still-unified TlsStream). mTLS verification itself already
/// happened during acceptor.accept() (with_client_cert_verifier); this
/// never makes its own trust decision, only reads what's already been
/// verified.
///
/// Returns None (never fails the connection) if the cert genuinely
/// has no DNS SAN at all -- rare/effectively impossible for certs
/// issued by cert-issuer itself, which always embeds one, but this is
/// an additive enhancement, not a new security gate, so failing open
/// here is deliberate.
fn extract_peer_identity(stream: &TlsStream<TcpStream>) -> Option<String> {
    let (_, connection) = stream.get_ref();
    let peer_certs = connection.peer_certificates()?;
    let leaf_cert = peer_certs.first()?;

    use x509_parser::prelude::FromDer;
    let (_, parsed) = match x509_parser::certificate::X509Certificate::from_der(leaf_cert.as_ref())
    {
        Ok(v) => v,
        Err(e) => {
            warn!(error = ?e, "Failed to parse peer certificate for identity extraction");
            return None;
        }
    };

    for ext in parsed.extensions() {
        if let x509_parser::extensions::ParsedExtension::SubjectAlternativeName(san) =
            ext.parsed_extension()
        {
            for name in &san.general_names {
                if let x509_parser::extensions::GeneralName::DNSName(dns) = name {
                    return Some(dns.to_string());
                }
            }
        }
    }
    None
}

/// The serial of the connecting client's verified certificate, as the credentials agent's ledger writes it, read once
/// right after the handshake, as the identity is: the broker refuses and ends a revoked certificate's sessions
/// (docs/architecture/key-custody.md, step 5). None if the certificate cannot be read, which the floor reports.
fn extract_peer_serial(stream: &TlsStream<TcpStream>) -> Option<String> {
    use x509_parser::prelude::FromDer;
    let (_, connection) = stream.get_ref();
    let leaf = connection.peer_certificates()?.first()?;
    let (_, parsed) = x509_parser::certificate::X509Certificate::from_der(leaf.as_ref()).ok()?;
    Some(cert_issuer_common::serial::serial_from_cert(&parsed))
}

/// What the connecting client's verified certificate admits where no policy speaks (its declared topics, and those
/// its workload and groups imply), read once, right after the handshake, as the identity is. A certificate that cannot
/// be read gives the reason, and every publish and subscribe on the connection is refused with it.
fn extract_peer_floor(stream: &TlsStream<TcpStream>) -> Result<Arc<Floor>, String> {
    let (_, connection) = stream.get_ref();
    let leaf = connection
        .peer_certificates()
        .and_then(|certs| certs.first())
        .ok_or_else(|| "the connection presented no certificate".to_string())?;
    Floor::from_certificate_der(leaf.as_ref())
        .map(Arc::new)
        .map_err(|e| format!("the connection's certificate cannot be read: {e}"))
}

/// A publish as its reader admitted it: its payload carrying the frame's charge, and the frame's window credit with
/// it until its topic accepts it.
fn charged(message: BrokerMessage, (credit, charge): (WindowCharge, Charge)) -> BrokerMessage {
    match message {
        BrokerMessage::PublishRequest {
            registration_id,
            topic,
            payload,
            reply_to,
            trace_ctx,
            published_by,
            id,
            ..
        } => BrokerMessage::PublishRequest {
            registration_id,
            topic,
            payload: {
                let weight = charge.bytes();
                Payload::charged(payload.bytes().clone(), Hold::new(charge), weight)
            },
            reply_to,
            trace_ctx,
            published_by,
            id,
            window: Hold::new(credit),
        },
        other => other,
    }
}

// ============================== Listener Manager ============================== //

pub struct ListenerManager;

pub struct ListenerManagerState {
    bind_addr: String,
    server_config: Arc<ServerConfig>,
    is_shutting_down: bool,
    terminating_listeners: bool,
    accept_task_abort: Option<tokio::task::AbortHandle>,
    shutdown_token: CancellationToken,
    listeners: HashMap<String, ActorRef<BrokerMessage>>, // client_id -> listener ref
    session_mgr: ActorRef<BrokerMessage>,
    authority: BrokerAuthority,
    bounds: Bounds,
}

pub struct ListenerManagerArgs {
    pub bind_addr: String,
    pub server_cert_file: String,
    pub server_key: polar_custody::client::ServerKey,
    pub ca_cert_file: String,
    pub session_mgr: ActorRef<BrokerMessage>,
    pub shutdown_auth_token: Option<String>,
    /// Who may publish and subscribe to what, which every Listener asks.
    pub authority: BrokerAuthority,
    /// The memory budget and each connection's window.
    pub bounds: Bounds,
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
        let span = debug_span!("cassini.listener_manager.init", actor = ?myself, bind_addr = %args.bind_addr);
        let _g = span.enter();

        let _shutdown_auth_token = args.shutdown_auth_token.clone();

        debug!("ListenerManager starting");

        // install default crypto provider
        let provider = rustls::crypto::ring::default_provider().install_default();
        if provider.is_err() {
            debug!("Crypto provider already configured");
        } else {
            debug!("Crypto provider configured");
        }

        debug!("Gathering certificates for mTLS");
        let certs: Vec<_> = CertificateDer::pem_file_iter(&args.server_cert_file)
            .map_err(ActorProcessingErr::from)?
            .map(|cert| cert.map_err(ActorProcessingErr::from))
            .collect::<Result<Vec<_>, _>>()?;

        let mut root_store = RootCertStore::empty();
        let root_cert = CertificateDer::from_pem_file(&args.ca_cert_file).map_err(|e| {
            error!(error = ?e, ca_cert_file = %args.ca_cert_file, "Failed reading CA cert PEM");
            ActorProcessingErr::from(e)
        })?;
        root_store
            .add(root_cert)
            .map_err(ActorProcessingErr::from)?;

        let verifier = WebPkiClientVerifier::builder(Arc::new(root_store))
            .build()
            .map_err(ActorProcessingErr::from)?;

        debug!("Building ServerConfig for mTLS");

        // A key file as before, or the custody daemon's server slot, which offers TLS 1.3 alone
        // (docs/architecture/key-custody.md, step 4).
        let server_config = polar_custody::client::server_config(verifier, certs, &args.server_key)
            .map_err(|e| {
                error!(error = %e, "Failed building the server configuration");
                ActorProcessingErr::from(e)
            })?;

        Ok(ListenerManagerState {
            bind_addr: args.bind_addr,
            server_config: Arc::new(server_config),
            is_shutting_down: false,
            accept_task_abort: None,
            shutdown_token: CancellationToken::new(),
            terminating_listeners: false,
            listeners: HashMap::new(),
            session_mgr: args.session_mgr,
            authority: args.authority,
            bounds: args.bounds,
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let span = info_span!(
            "cassini.listener_manager.serve",
            actor = ?myself,
            bind_addr = %state.bind_addr
        );

        async {
            let bind_addr = state.bind_addr.clone();
            let acceptor = TlsAcceptor::from(Arc::clone(&state.server_config));
            let server = TcpListener::bind(&bind_addr)
                .await
                .map_err(ActorProcessingErr::from)?;

            info!("Server running on {bind_addr}");
            let shutdown_token = state.shutdown_token.clone();
            let session_mgr = state.session_mgr.clone(); // clone for use in spawned tasks
            let authority = state.authority.clone();
            let bounds = state.bounds.clone();

            // Accept loop runs in its own task; do NOT keep an entered span alive forever.
            let join_handle = tokio::spawn({
                let myself = myself.clone();
                let session_mgr = session_mgr; // move into closure
                let authority = authority;
                let bounds = bounds;
                async move {
                    loop {
                        let (stream, peer_addr) = match server.accept().await {
                            Ok(v) => v,
                            Err(e) => {
                                // Some accept() failures (e.g. EMFILE/ENFILE when the
                                // process is out of file descriptors) return
                                // immediately rather than blocking. Without a
                                // backoff, `continue` here spins this task at
                                // 100% CPU on one core and floods the logs for as
                                // long as the resource stays exhausted.
                                warn!(error = %e, "TCP accept failed");
                                tokio::time::sleep(Duration::from_millis(100)).await;
                                continue;
                            }
                        };

                        // Find a client that has died without a word in about a minute, instead of
                        // keeping its session, and everything held for it, until a write fails.
                        if let Err(e) = cassini_types::socket::enable_dead_peer_detection(&stream) {
                            warn!(error = %e, %peer_addr, "could not enable dead-peer detection");
                        }

                        // Check if we're shutting down before accepting new connection
                        if shutdown_token.is_cancelled() {
                            debug!(
                                "ListenerManager not accepting new connections, stopping accept loop"
                            );
                            break;
                        }

                        let handshake_span = info_span!(
                            "cassini.listener_manager.accept",
                            peer_addr = %peer_addr
                        );

                        let acceptor = acceptor.clone();
                        let myself = myself.clone();
                        let session_mgr = session_mgr.clone(); // clone again for inner spawn
                        let authority = authority.clone();
                        let bounds = bounds.clone();

                        tokio::spawn(
                            async move {
                                let stream = match acceptor.accept(stream).await {
                                    Ok(s) => s,
                                    Err(e) => {
                                        warn!(error = %e, "TLS handshake failed");
                                        return;
                                    }
                                };
                                // Extracted before split() -- peer_certificates()
                                // is only reachable through the still-unified
                                // TlsStream.
                                let peer_identity = extract_peer_identity(&stream);
                                let peer_serial = extract_peer_serial(&stream);
                                let floor = extract_peer_floor(&stream);
                                if let Err(why) = &floor {
                                    warn!(%why, "every publish and subscribe on this connection will be refused");
                                }
                                let client_id = Uuid::new_v4().to_string();
                                let (reader, writer) = split(stream);
                                let writer = ConnWriter::new(client_id.clone(), BufWriter::new(writer));
                                let listener_args = ListenerArguments {
                                    writer,
                                    reader: Some(reader),
                                    client_id: client_id.clone(),
                                    registration_id: None,
                                    session_mgr,
                                    broker: myself
                                        .try_get_supervisor()
                                        .expect("ListenerManager must have a supervisor (the broker)")
                                        .into(),
                                    peer_identity,
                                    peer_serial,
                                    floor,
                                    authority,
                                    bounds,
                                };

                                let spawn_span = info_span!(
                                    "cassini.listener_manager.spawn_listener",
                                    client_id = %client_id
                                );
                                async {
                                    match Actor::spawn_linked(
                                        Some(client_id.clone()),
                                        Listener,
                                        listener_args,
                                        myself.clone().into(),
                                    )
                                    .await
                                    {
                                        Ok((listener_ref, _)) => {
                                            let _ = myself.send_message(BrokerMessage::RegisterListener {
                                                client_id: client_id.clone(),
                                                listener_ref,
                                            });
                                        }
                                        Err(e) => {
                                            error!(error = ?e, "Failed to spawn Listener actor for connection {}", client_id);
                                        }
                                }
                                }
                                .instrument(spawn_span)
                                .await;
                            }
                            .instrument(handshake_span),
                        );
                    }
                }
            });

            state.accept_task_abort = Some(join_handle.abort_handle());

            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span)
        .await
    }

    /// However the manager ends, it stops listening and closes its connections: the accept loop is
    /// a task of its own, and left running it would keep the address bound.
    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        state.shutdown_token.cancel();
        if let Some(handle) = state.accept_task_abort.take() {
            handle.abort();
        }
        // A connection's reading task outlives a listener that is killed rather than stopped, and
        // while it does the socket stays open and the client never learns the broker is gone.
        myself
            .get_cell()
            .stop_children_and_wait(None, Some(std::time::Duration::from_secs(5)))
            .await;
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(actor_cell) => {
                debug!(
                    worker_name = ?actor_cell.get_name(),
                    worker_id = ?actor_cell.get_id(),
                    "Worker started"
                );
                if actor_cell.get_name().is_some() {
                    // Actor ref will be registered via RegisterListener message after spawn.
                }
            }
            SupervisionEvent::ActorTerminated(actor_cell, _, _) => {
                if let Some(name) = actor_cell.get_name() {
                    state.listeners.remove(&name);
                    // If we are in TerminateListeners phase and no listeners remain, stop manager
                    if state.terminating_listeners && state.listeners.is_empty() {
                        if let Some(supervisor) = myself.try_get_supervisor() {
                            let _ = supervisor.send_message(BrokerMessage::ShutdownPhaseComplete {
                                phase: ShutdownPhase::TerminateListeners,
                            });
                        }
                        myself.stop(Some("SHUTDOWN_LISTENERS_TERMINATED".to_string()));
                    }
                }
            }
            _ => (),
        }
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<BrokerMessage>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            // Phase 1: stop accepting new connections, but keep existing listeners alive
            BrokerMessage::InitiateShutdownPhase {
                phase: ShutdownPhase::StopAcceptingNewConnections,
            } => {
                self.handle_initiate_shutdown_phase_stop_accepting(myself.clone(), state)
                    .await?;
            }
            // Final phase: terminate all listener actors and then stop
            BrokerMessage::InitiateShutdownPhase {
                phase: ShutdownPhase::TerminateListeners,
            } => {
                self.handle_initiate_shutdown_phase_terminate_listeners(myself.clone(), state)
                    .await?;
            }
            BrokerMessage::RegisterListener {
                client_id,
                listener_ref,
            } => {
                state.listeners.insert(client_id, listener_ref);
            }
            BrokerMessage::JudgeSubscriptions { topics } => {
                for listener in state.listeners.values() {
                    let _ = listener.send_message(BrokerMessage::JudgeSubscriptions {
                        topics: topics.clone(),
                    });
                }
            }
            BrokerMessage::RegistrationResponse {
                client_id,
                result,
                trace_ctx,
            } => {
                self.handle_registration_response(
                    myself.clone(),
                    state,
                    client_id,
                    result,
                    trace_ctx,
                )
                .await?;
            }
            BrokerMessage::DisconnectRequest {
                reason,
                client_id,
                registration_id,
                trace_ctx,
            } => {
                self.handle_disconnect_request(
                    myself.clone(),
                    state,
                    reason,
                    client_id,
                    registration_id,
                    trace_ctx,
                )
                .await?;
            }
            _ => (),
        }
        Ok(())
    }
}

impl ListenerManager {
    // ===== Handler methods for ListenerManager =====
    async fn handle_initiate_shutdown_phase_stop_accepting(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut ListenerManagerState,
    ) -> Result<(), ActorProcessingErr> {
        if state.is_shutting_down {
            warn!("ListenerManager already shutting down");
            return Ok(());
        }

        info!("ListenerManager: stopping new connections");
        state.is_shutting_down = true;

        // Cancel the accept loop
        state.shutdown_token.cancel();
        if let Some(handle) = state.accept_task_abort.take() {
            handle.abort();
        }

        // Signal that we have completed the phase
        if let Some(supervisor) = myself.try_get_supervisor() {
            supervisor.send_message(BrokerMessage::ShutdownPhaseComplete {
                phase: ShutdownPhase::StopAcceptingNewConnections,
            })?;
        }
        // DO NOT STOP the manager; keep it alive for the final phase.
        Ok(())
    }

    async fn handle_initiate_shutdown_phase_terminate_listeners(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut ListenerManagerState,
    ) -> Result<(), ActorProcessingErr> {
        if state.terminating_listeners {
            return Ok(());
        }
        info!("ListenerManager: terminating all listener actors");
        state.terminating_listeners = true;

        // Stop all active listeners
        for (client_id, listener_ref) in &state.listeners {
            let listener = listener_ref.clone();
            listener.stop(Some("SHUTDOWN_TERMINATE_LISTENER".to_string()));
            debug!("Stopped listener {}", client_id);
        }

        // If no listeners were active, complete immediately
        if state.listeners.is_empty() {
            if let Some(supervisor) = myself.try_get_supervisor() {
                let _ = supervisor.send_message(BrokerMessage::ShutdownPhaseComplete {
                    phase: ShutdownPhase::TerminateListeners,
                });
            }
            myself.stop(Some("SHUTDOWN_NO_LISTENERS".to_string()));
        }
        // Otherwise, wait for ActorTerminated events to trigger completion
        Ok(())
    }

    async fn handle_registration_response(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerManagerState,
        client_id: String,
        result: Result<String, String>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        // Reject new registrations during shutdown
        if state.is_shutting_down {
            warn!("Rejecting registration response during shutdown");
            return Ok(());
        }

        let span = trace_span!("cassini.request.registration", %client_id);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();

        trace!("Forwarding registration response to listener");

        if let Some(listener_ref) = state.listeners.get(&client_id) {
            if let Err(e) = listener_ref.send_message(BrokerMessage::RegistrationResponse {
                client_id,
                result,
                trace_ctx: Some(span.context()),
            }) {
                error!(error = %e, "Failed to forward RegistrationResponse to listener");
            }
        } else {
            warn!("Couldn't find listener {} in listeners map", client_id);
        }
        Ok(())
    }

    async fn handle_disconnect_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerManagerState,
        reason: DisconnectReason,
        client_id: String,
        registration_id: Option<String>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.listener_manager.disconnect_request", %client_id, registration_id = ?registration_id, reason = ?reason);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("ListenerManager handling disconnect");

        // The session manager decides what a disconnect means (a goodbye ends the session, anything
        // else keeps it for a while). It used to be sent to the session actor, which does not
        // handle it and logged it as unexpected.
        if let Some(id) = registration_id {
            let _ = state
                .session_mgr
                .send_message(BrokerMessage::DisconnectRequest {
                    reason,
                    client_id: client_id.clone(),
                    registration_id: Some(id),
                    trace_ctx: Some(span.context()),
                });
        } else {
            warn!("DisconnectRequest missing registration_id");
        }

        // Optionally stop the listener if it still exists (it is already stopping itself)
        if let Some(listener_ref) = state.listeners.get(&client_id) {
            listener_ref.stop(None);
            debug!("Stopped listener {}", client_id);
        }

        // If we are in TerminateListeners phase and this listener was the last one,
        // the ActorTerminated handler will take care of completion.
        Ok(())
    }
}

// ============================== Listener actor ============================== //

struct Listener;

struct ListenerState {
    writer: SharedWriter,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    client_id: String,
    session_mgr: ActorRef<BrokerMessage>,
    session_ref: Option<ActorRef<BrokerMessage>>,
    registration_id: Option<String>,
    task_handle: Option<JoinHandle<()>>,
    broker: ActorRef<BrokerMessage>,
    peer_identity: Option<String>,
    /// The serial of this connection's certificate, checked against the revoked set.
    peer_serial: Option<String>,
    /// What this connection's certificate admits where no policy speaks, or why it cannot be read.
    floor: Result<Arc<Floor>, String>,
    authority: BrokerAuthority,
    /// This connection's recent decisions (no lock is shared with any other connection).
    decisions: DecisionCache,
    /// The topics the broker has acknowledged this client's subscription to, which are judged again on a change of
    /// authority.
    subscribed: std::collections::HashSet<String>,
    /// Subscriptions the broker dropped itself: the client was told it was refused, so the acknowledgment of the
    /// unsubscribe is not passed on.
    dropped: std::collections::HashSet<String>,
    /// The deliveries written to this client and not yet acknowledged, by number, with the topic each came on.
    deliveries: HashMap<u64, String>,
    /// The frame each of those was written in, and the delivery each such frame carried, so a frame its client could
    /// not decode can be named (`ClientMessage::FrameUndecodable`).
    frame_of: HashMap<u64, u64>,
    delivery_in: HashMap<u64, u64>,
    bounds: Bounds,
}

struct ListenerArguments {
    writer: SharedWriter,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    client_id: String,
    session_mgr: ActorRef<BrokerMessage>,
    registration_id: Option<String>,
    broker: ActorRef<BrokerMessage>,
    peer_identity: Option<String>,
    peer_serial: Option<String>,
    floor: Result<Arc<Floor>, String>,
    authority: BrokerAuthority,
    bounds: Bounds,
}

impl Listener {
    /// Whether this connection's certificate is among those the credentials agent says are revoked.
    fn is_revoked(state: &ListenerState) -> bool {
        state
            .peer_serial
            .as_deref()
            .is_some_and(|serial| state.authority.handle().revoked().contains(serial))
    }

    /// Whether this connection may use `topic` this way: the broker's policy first, then the certificate's floor,
    /// refused by default. `Err` is the reason, which the client is told.
    /// A decision this connection made a moment ago, under the same authority, is not made again.
    async fn decide(state: &mut ListenerState, topic: &str, access: Access) -> Result<(), String> {
        let floor = match &state.floor {
            Ok(floor) => floor.clone(),
            Err(why) => return Err(format!("the broker refused: {why}")),
        };
        // Read before deciding: a change while the decision is made leaves what it keeps stale.
        let changes = state.authority.changes();
        if let Some(kept) = state
            .decisions
            .get(topic, access, std::time::Instant::now(), changes)
        {
            return kept;
        }
        let decided = state.authority.decide(&floor, topic, access).await;
        state.decisions.put(
            topic,
            access,
            decided.clone(),
            std::time::Instant::now(),
            changes,
        );
        decided
    }

    /// Ends a subscription the broker no longer admits: the session unsubscribes, and the client is told, unasked,
    /// that it is refused (`SubscribeRefused`), so it can keep it as wanted and ask again when its authority changes.
    async fn drop_subscription(state: &mut ListenerState, topic: String, reason: String) {
        if !state.subscribed.remove(&topic) && state.dropped.contains(&topic) {
            return;
        }
        warn!(%topic, identity = ?state.peer_identity, %reason, "a held subscription is no longer admitted: dropped");
        state.dropped.insert(topic.clone());
        if let Some(registration_id) = state.registration_id.clone()
            && let Some(session) = where_is(registration_id.clone())
        {
            let _ = session.send_message(BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic: topic.clone(),
                trace_ctx: None,
            });
        }
        let msg = ClientMessage::SubscribeAcknowledgment {
            topic,
            result: Err(reason),
            trace_ctx: WireTraceCtx::from_current_span(),
        };
        if let Err(e) =
            Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await
        {
            error!(error = ?e, "Failed to tell the client a subscription was dropped");
        }
    }

    /// Write a framed ClientMessage to the peer.
    ///
    /// Header is **u32 big-endian length**, followed by payload bytes.
    #[instrument(level = "trace", skip(writer, message), fields(client_id = %client_id))]
    /// Writes `message` as one frame, and returns its number on this connection.
    async fn write(
        client_id: String,
        message: ClientMessage,
        writer: SharedWriter,
    ) -> Result<u64, Error> {
        let bytes = rkyv::to_bytes::<Error>(&message)?;

        let len_u32: u32 = bytes.len().try_into().map_err(|_| {
            rancor::Error::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame too large for u32 length",
            ))
        })?;

        trace!(
            msg_ty = %std::any::type_name::<ClientMessage>(),
            len = bytes.len(),
            first_8 = ?&bytes[..bytes.len().min(8)],
            "Sending frame"
        );

        // A write waits on a client that does not read for as long as it does not; it gives up when the connection
        // is cut (`cut_connection`).
        let written = async {
            let mut w = writer.half.lock().await;
            let frame = writer.frames.fetch_add(1, Ordering::SeqCst) + 1;
            w.write_all(&len_u32.to_be_bytes()).await?;
            w.write_all(&bytes).await?;
            w.flush().await.map(|()| frame)
        };
        tokio::select! {
            written = written => written.map_err(rancor::Error::new),
            _ = writer.cut.cancelled() => Err(rancor::Error::new(std::io::Error::other(writer.why()))),
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
        myself: ActorRef<Self::Msg>,
        args: ListenerArguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!(
            actor = ?myself,
            client_id = %args.client_id,
            "Listener pre_start"
        );

        Ok(ListenerState {
            writer: args.writer,
            reader: args.reader,
            client_id: args.client_id,
            session_mgr: args.session_mgr,
            session_ref: None,
            registration_id: args.registration_id,
            task_handle: None,
            broker: args.broker,
            peer_identity: args.peer_identity,
            peer_serial: args.peer_serial,
            floor: args.floor,
            decisions: args.authority.cache(),
            authority: args.authority,
            subscribed: std::collections::HashSet::new(),
            dropped: std::collections::HashSet::new(),
            deliveries: HashMap::new(),
            frame_of: HashMap::new(),
            delivery_in: HashMap::new(),
            bounds: args.bounds,
        })
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let Some(handle) = state.task_handle.take() {
            handle.abort();
        }
        Ok(())
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // short-lived startup span only
        {
            let span = info_span!("cassini.listener.start", client_id = %state.client_id);
            let _g = span.enter();
            info!("Listener actor started");
        }

        let client_id = state.client_id.clone();
        let peer_identity = state.peer_identity.clone();
        let reader = state.reader.take().ok_or_else(|| {
            ActorProcessingErr::from(std::io::Error::other("Reader already taken"))
        })?;

        // Read loop: NO long-lived span, and no span entered across a wait (the task can resume on another thread,
        // whose span stack it is not on). Each frame's span instruments what waits.
        //
        // A frame larger than `SMALL_FRAME` is admitted before it is read: its bytes are taken from this connection's
        // window and from the broker's budget, and while either cannot give them nothing more is read, so TCP pushes
        // back on the client. A smaller one is read first, so acknowledgements and subscriptions are never stuck
        // behind the frames they make room for; a publish among them is admitted before it goes on, and while it waits
        // the reader goes on reading: acknowledgements after it are passed on at once, and the first other frame is
        // kept until the publish is admitted (a large one's body is not read). A publish carries its charge (the
        // frame's) and its window credit (released when its topic accepts it). What still waits (a large frame, or a
        // frame behind the one kept) is bounded by the stall bound: the budget's watchdog ends the connections of the
        // subscribers that hold the budget and make no progress (budget.rs, `spawn_watchdog`).
        let window = Window::of(&state.bounds.budget);
        let budget = state.bounds.budget.clone();
        let stall = state.bounds.stall;
        let writer = state.writer.clone();
        let handle = tokio::spawn({
            let myself = myself.clone();
            async move {
                let mut buf_reader = tokio::io::BufReader::new(reader);
                let cut = writer.cut.clone();
                let disconnect = |reason: DisconnectReason| {
                    let _ = myself.send_message(BrokerMessage::DisconnectRequest {
                        reason,
                        client_id: client_id.clone(),
                        registration_id: None,
                        trace_ctx: None,
                    });
                };

                // A frame read while a publish waited for admission, to go on with.
                let mut next: Option<Next> = None;
                // A registration of this broker's protocol has been read: until then a frame that does not decode
                // is most likely an older client's registration, and is refused as a protocol mismatch.
                let mut registered = false;
                let refuse = |refusal: cassini_types::Refusal| {
                    let _ = myself.send_message(BrokerMessage::RefuseRegistration { refusal });
                };
                loop {
                    let (len, read) = match next.take() {
                        Some(Next::Header(len)) => (len, None),
                        Some(Next::Frame(len, message)) => (len, Some(message)),
                        None => {
                            match read_header(&mut buf_reader, &cut, &client_id, &peer_identity)
                                .await
                            {
                                Ok(0) => continue,
                                Ok(len) => (len, None),
                                Err(gone) => {
                                    disconnect(gone.reason(&writer));
                                    return;
                                }
                            }
                        }
                    };

                    let frame_span = trace_span!(
                        "cassini.listener.read_frame",
                        client_id = %client_id,
                        frame_len = len
                    );

                    let mut admitted = None;
                    // A frame refused for now, and the slot its body is read under, held until it is let go.
                    let mut refused = false;
                    let mut _refused_body = None;
                    let deserialized = match read {
                        Some(message) => message,
                        None => {
                            if len > SMALL_FRAME {
                                match admit_or_refuse(&window, &budget, len, &cut, &writer)
                                    .instrument(frame_span.clone())
                                    .await
                                {
                                    Admission::Admitted(admission) => admitted = Some(admission),
                                    Admission::Refused => refused = true,
                                    Admission::Cut => {
                                        disconnect(Gone::Cut.reason(&writer));
                                        return;
                                    }
                                }
                            }
                            let body = if refused {
                                // Read past, under the one slot, within the stall bound, and let go.
                                _refused_body = tokio::select! {
                                    permit = REFUSED_BODY.acquire() => permit.ok(),
                                    _ = cut.cancelled() => {
                                        disconnect(Gone::Cut.reason(&writer));
                                        return;
                                    }
                                };
                                match tokio::time::timeout(
                                    stall,
                                    read_body(&mut buf_reader, len, &cut, &frame_span),
                                )
                                .await
                                {
                                    Ok(body) => body,
                                    Err(_) => Err(Gone::Failed(format!(
                                        "a frame refused for now was not sent within the stall bound ({stall:?})"
                                    ))),
                                }
                            } else {
                                read_body(&mut buf_reader, len, &cut, &frame_span).await
                            };
                            match body {
                                Ok(Some(message)) => message,
                                Ok(None) if !registered => {
                                    refuse(cassini_types::Refusal::undecodable_registration());
                                    return;
                                }
                                Ok(None) => continue,
                                Err(gone) => {
                                    disconnect(gone.reason(&writer));
                                    return;
                                }
                            }
                        }
                    };

                    if let ClientMessage::RegistrationRequest { protocol, .. } = &deserialized {
                        if *protocol != cassini_types::PROTOCOL_VERSION {
                            refuse(cassini_types::Refusal::protocol(*protocol));
                            return;
                        }
                        registered = true;
                    }
                    if matches!(
                        deserialized,
                        ClientMessage::PublishResponseAck { .. }
                            | ClientMessage::FrameUndecodable { .. }
                    ) {
                        writer.progressed();
                    }
                    let publish = match &deserialized {
                        ClientMessage::PublishRequest {
                            topic, epoch, seq, ..
                        } => Some((
                            topic.clone(),
                            MessageId {
                                epoch: *epoch,
                                seq: *seq,
                            },
                        )),
                        _ => None,
                    };
                    let is_publish = publish.is_some();
                    // Convert to broker message and forward to actor handler.
                    let mut converted = BrokerMessage::from_client_message(
                        deserialized,
                        client_id.clone(),
                        peer_identity.clone(),
                    );
                    if is_publish {
                        let admission = match admitted.take() {
                            Some(admission) => Some(admission),
                            None if refused => None,
                            // A small publish waits for its admission while what follows it is read: an
                            // acknowledgement goes on at once (it may be what makes the room), and the first frame
                            // that is not one is kept for after, when reading stops until the publish is admitted.
                            None => {
                                let mut admission = std::pin::pin!(
                                    admit_or_refuse(&window, &budget, len, &cut, &writer)
                                        .instrument(frame_span.clone())
                                );
                                loop {
                                    tokio::select! {
                                        biased;
                                        admission = &mut admission => match admission {
                                            Admission::Admitted(admission) => break Some(admission),
                                            Admission::Refused => break None,
                                            Admission::Cut => {
                                                disconnect(Gone::Cut.reason(&writer));
                                                return;
                                            }
                                        },
                                        filled = async { buf_reader.fill_buf().await.map(|b| b.is_empty()) },
                                            if next.is_none() =>
                                        {
                                            match filled {
                                                Ok(false) => {}
                                                Ok(true) => {
                                                    disconnect(DisconnectReason::RemoteClosed);
                                                    return;
                                                }
                                                Err(e) => {
                                                    disconnect(DisconnectReason::TransportError(e.to_string()));
                                                    return;
                                                }
                                            }
                                            let following = match read_header(&mut buf_reader, &cut, &client_id, &peer_identity).await {
                                                Ok(0) => continue,
                                                Ok(following) => following,
                                                Err(gone) => {
                                                    disconnect(gone.reason(&writer));
                                                    return;
                                                }
                                            };
                                            if following > SMALL_FRAME {
                                                next = Some(Next::Header(following));
                                                continue;
                                            }
                                            let span = trace_span!(
                                                "cassini.listener.read_frame",
                                                client_id = %client_id,
                                                frame_len = following
                                            );
                                            match read_body(&mut buf_reader, following, &cut, &span).await {
                                                Ok(Some(
                                                    ack @ (ClientMessage::PublishResponseAck { .. }
                                                    | ClientMessage::FrameUndecodable { .. }),
                                                )) => {
                                                    writer.progressed();
                                                    let ack = BrokerMessage::from_client_message(
                                                        ack,
                                                        client_id.clone(),
                                                        peer_identity.clone(),
                                                    );
                                                    if let Err(e) = myself.send_message(ack) {
                                                        warn!(error = %e, "Failed forwarding an acknowledgement to listener actor");
                                                    }
                                                }
                                                Ok(Some(message)) => next = Some(Next::Frame(following, message)),
                                                Ok(None) => {}
                                                Err(gone) => {
                                                    disconnect(gone.reason(&writer));
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        };
                        match admission {
                            Some(admission) => converted = charged(converted, admission),
                            None => {
                                // Refused for now: its client keeps it and sends it again; this reader reads on.
                                let (topic, id) = publish.expect("a publish");
                                debug!(%topic, ?id, "a publish refused for now: its connection's acknowledgements wait behind it");
                                let refusal = BrokerMessage::PublishResponse {
                                    topic,
                                    payload: Payload::uncharged(Arc::new(Vec::new())),
                                    result: Err(REFUSED_FOR_NOW.to_string()),
                                    trace_ctx: None,
                                    published_by: None,
                                    origin: Some(id),
                                    delivery_id: None,
                                    refusal: Some(cassini_types::PublishRefusal::Unavailable),
                                };
                                if let Err(e) = myself.send_message(refusal) {
                                    warn!(error = %e, "Failed forwarding a refusal for now to listener actor");
                                }
                                continue;
                            }
                        }
                    }
                    // Anything else admitted gives its bytes back now.
                    drop(admitted);

                    if let Err(e) = myself.send_message(converted) {
                        warn!(error = %e, "Failed forwarding decoded message to listener actor");
                    }
                }
            }
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
            // A registration this broker cannot take for its protocol: told by name, and the connection closed.
            BrokerMessage::RefuseRegistration { refusal } => {
                warn!(
                    client_id = %state.client_id,
                    identity = ?state.peer_identity,
                    code = ?refusal.code,
                    reason = %refusal.reason,
                    "a registration of another protocol version is refused"
                );
                let msg = ClientMessage::RegistrationResponse {
                    result: Err(refusal.clone()),
                    trace_ctx: None,
                };
                if let Err(e) =
                    Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await
                {
                    debug!(error = ?e, "the refusal of a registration was not written");
                }
                myself.stop(Some(refusal.reason));
            }
            BrokerMessage::SetListenerSessionRef { session_ref } => {
                // The session is given its connection, and from now on its reader tells it what it reads and when it
                // waits, beside this Listener's mailbox (which waits behind its writes).
                let _ = session_ref.send_message(BrokerMessage::ConnectionOpened {
                    client_id: state.client_id.clone(),
                    control: Arc::new(ConnHandle(Arc::downgrade(&state.writer))),
                });
                *state.writer.session.lock().unwrap() = Some(session_ref.clone());
                state.session_ref = Some(session_ref);
            }
            BrokerMessage::JudgeSubscriptions { topics } => {
                // Sent on every change of authority: a certificate revoked since is closed. Its session is kept for the
                // retention window, as after any lost connection, for the identity's next certificate to claim; this
                // one cannot resume it, refused at registration.
                if Self::is_revoked(state) {
                    warn!(identity = ?state.peer_identity, serial = ?state.peer_serial, "the connection's certificate is revoked: closing it");
                    let client_id = state.client_id.clone();
                    return self
                        .handle_disconnect_request(
                            myself,
                            state,
                            DisconnectReason::RemoteClosed,
                            client_id,
                            None,
                            None,
                        )
                        .await;
                }
                let topics: Vec<String> = match topics {
                    Some(resumed) => {
                        state.subscribed.extend(resumed.iter().cloned());
                        resumed
                    }
                    None => state.subscribed.iter().cloned().collect(),
                };
                for topic in topics {
                    if let Err(reason) = Self::decide(state, &topic, Access::Subscribe).await {
                        Self::drop_subscription(state, topic, reason).await;
                    }
                }
            }
            BrokerMessage::RegistrationRequest {
                registration_id,
                client_id,
                trace_ctx,
                peer_identity,
            } => {
                if Self::is_revoked(state) {
                    warn!(identity = ?state.peer_identity, serial = ?state.peer_serial, "a revoked certificate asked to register: refused, and closed");
                    let msg = ClientMessage::RegistrationResponse {
                        result: Err(cassini_types::Refusal::revoked()),
                        trace_ctx: WireTraceCtx::from_current_span(),
                    };
                    if let Err(e) =
                        Listener::write(client_id.clone(), msg, Arc::clone(&state.writer)).await
                    {
                        error!(error = ?e, "Failed to tell a revoked certificate it is refused");
                    }
                    myself.stop(Some("REVOKED".to_string()));
                    return Ok(());
                }
                self.handle_registration_request(
                    myself,
                    state,
                    registration_id,
                    client_id,
                    trace_ctx,
                    peer_identity,
                )
                .await?;
            }
            BrokerMessage::RegistrationResponse {
                client_id,
                result,
                trace_ctx,
            } => {
                self.handle_registration_response(myself, state, client_id, result, trace_ctx)
                    .await?;
            }
            BrokerMessage::DisconnectRequest {
                reason,
                client_id,
                registration_id,
                trace_ctx,
            } => {
                self.handle_disconnect_request(
                    myself,
                    state,
                    reason,
                    client_id,
                    registration_id,
                    trace_ctx,
                )
                .await?;
            }
            // When the manager tells us to stop (final phase), we just stop.
            BrokerMessage::PrepareForShutdown { .. } => {
                self.handle_prepare_for_shutdown(myself, state).await?;
            }
            BrokerMessage::PublishRequest {
                topic,
                payload,
                registration_id,
                reply_to,
                trace_ctx,
                published_by,
                id,
                window,
            } => {
                self.handle_publish_request(
                    myself,
                    state,
                    topic,
                    payload,
                    registration_id,
                    reply_to,
                    trace_ctx,
                    published_by,
                    id,
                    window,
                )
                .await?;
            }
            BrokerMessage::PublishResponse {
                topic,
                payload,
                trace_ctx,
                result,
                published_by,
                origin,
                delivery_id,
                refusal,
            } => {
                self.handle_publish_response(
                    myself,
                    state,
                    topic,
                    payload,
                    trace_ctx,
                    result,
                    published_by,
                    origin,
                    delivery_id,
                    refusal,
                )
                .await?;
            }
            BrokerMessage::PublishResponseAck { delivery_id } => {
                Self::handle_delivery_ack(state, delivery_id);
            }
            // The client will never decode it: a delivery is let go as acknowledged, or it would be held, and sent
            // again on every resume, for good (cassini-backpressure.md, second review, finding B).
            BrokerMessage::FrameUndecodable { frame } => {
                match state.delivery_in.get(&frame).copied() {
                    Some(delivery_id) => {
                        warn!(
                            client_id = %state.client_id,
                            identity = ?state.peer_identity,
                            frame,
                            delivery_id,
                            "the client could not decode a delivery: it is let go as acknowledged"
                        );
                        Self::handle_delivery_ack(state, delivery_id);
                    }
                    None => warn!(
                        client_id = %state.client_id,
                        identity = ?state.peer_identity,
                        frame,
                        "the client could not decode a frame that was not a delivery it holds"
                    ),
                }
            }
            BrokerMessage::PublishRequestAck {
                topic,
                id,
                trace_ctx,
            } => {
                self.handle_publish_request_ack(myself, state, topic, id, trace_ctx)
                    .await?;
            }
            BrokerMessage::SubscribeAcknowledgment {
                registration_id,
                topic,
                trace_ctx,
                result,
            } => {
                self.handle_subscribe_ack(myself, state, registration_id, topic, trace_ctx, result)
                    .await?;
            }
            BrokerMessage::SubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                self.handle_subscribe_request(myself, state, registration_id, topic, trace_ctx)
                    .await?;
            }
            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                self.handle_unsubscribe_request(myself, state, registration_id, topic, trace_ctx)
                    .await?;
            }
            BrokerMessage::UnsubscribeAcknowledgment {
                registration_id: _,
                topic,
                result,
            } => {
                self.handle_unsubscribe_ack(myself, state, topic, result)
                    .await?;
            }
            BrokerMessage::ControlRequest {
                registration_id,
                op,
                trace_ctx,
                ..
            } => {
                self.handle_control_request(myself, state, registration_id, op, trace_ctx)
                    .await?;
            }
            BrokerMessage::ControlResponse {
                registration_id,
                result,
                trace_ctx,
            } => {
                self.handle_control_response(myself, state, registration_id, result, trace_ctx)
                    .await?;
            }
            other => {
                warn!(?other, "{UNEXPECTED_MESSAGE_STR}");
            }
        }

        Ok(())
    }
}

impl Listener {
    // ===== Handler methods for Listener =====
    async fn handle_registration_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        registration_id: Option<String>,
        client_id: String,
        trace_ctx: Option<opentelemetry::Context>,
        peer_identity: Option<String>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.request.registration", %client_id, registration_id = ?registration_id);
        try_set_parent_otel(&span, trace_ctx);
        async {
            trace!("Listener received registration request");
            let broker = state.broker.clone();
            if let Err(e) = broker.send_message(BrokerMessage::RegistrationRequest {
                registration_id,
                client_id: client_id.clone(),
                trace_ctx: Some(span.context()),
                peer_identity,
            }) {
                let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {e}");
                error!("{err_msg}");

                let msg = ClientMessage::RegistrationResponse {
                    result: Err(cassini_types::Refusal::other(err_msg.clone())),
                    trace_ctx: WireTraceCtx::from_current_span(),
                };

                if let Err(e) =
                    Listener::write(client_id.clone(), msg, Arc::clone(&state.writer)).await
                {
                    error!(error = ?e, "Failed to write RegistrationResponse");
                }

                myself.stop(Some(err_msg));
            } // Note: the else branch for broker not found is removed because broker is guaranteed
            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span.clone())
        .await
    }

    async fn handle_registration_response(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        client_id: String,
        result: Result<String, String>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.listener.registration_response", %client_id);
        try_set_parent_otel(&span, trace_ctx);
        async {
            trace!("Listener received registration response");

            match result {
                Ok(registration_id) => {
                    let reg_id_clone = registration_id.clone();
                    state.registration_id = Some(registration_id.clone());

                    let msg = ClientMessage::RegistrationResponse {
                        result: Ok(registration_id.clone()),
                        trace_ctx: WireTraceCtx::from_current_span(),
                    };

                    if let Err(e) =
                        Listener::write(client_id.clone(), msg, Arc::clone(&state.writer)).await
                    {
                        error!(error = ?e, "Failed to write RegistrationResponse");
                        let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                            client_id: state.client_id.clone(),
                            registration_id: state.registration_id.clone().unwrap_or_default(),
                            error: Some(e.to_string()),
                        });
                    } else {
                        let session_mgr = state.session_mgr.clone();
                        let myself_clone = myself.clone();
                        tokio::spawn(async move {
                            let result = session_mgr
                                .call(
                                    |reply_to| BrokerMessage::GetSessionRef {
                                        registration_id: reg_id_clone.clone(),
                                        reply_to,
                                    },
                                    Some(Duration::from_millis(100)),
                                )
                                .await;
                            match result {
                                Ok(CallResult::Success(Some(session_ref))) => {
                                    let _ = myself_clone.send_message(
                                        BrokerMessage::SetListenerSessionRef { session_ref },
                                    );
                                    debug!("Cached session ref for listener");
                                    // A resumed session keeps the subscriptions it had, made under the certificate it
                                    // had: they are judged again under this connection's.
                                    if let Some(subscribers) =
                                        where_is(crate::SUBSCRIBER_MANAGER_NAME.to_string())
                                        && let Ok(CallResult::Success(topics)) =
                                            ActorRef::<BrokerMessage>::from(subscribers)
                                                .call(
                                                    |reply_to| BrokerMessage::SubscriptionsOf {
                                                        registration_id: reg_id_clone.clone(),
                                                        reply_to,
                                                    },
                                                    Some(Duration::from_secs(2)),
                                                )
                                                .await
                                        && !topics.is_empty()
                                    {
                                        let _ = myself_clone.send_message(
                                            BrokerMessage::JudgeSubscriptions {
                                                topics: Some(topics),
                                            },
                                        );
                                    }
                                }
                                _ => {
                                    error!(
                                        "Failed to get session ref for registration {}",
                                        reg_id_clone
                                    );
                                }
                            }
                        });
                    }
                }
                Err(error) => {
                    let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {error}");
                    let msg = ClientMessage::RegistrationResponse {
                        result: Err(cassini_types::Refusal::other(err_msg.clone())),
                        trace_ctx: WireTraceCtx::from_current_span(),
                    };

                    if let Err(e) =
                        Listener::write(client_id.clone(), msg, Arc::clone(&state.writer)).await
                    {
                        error!(error = ?e, "Failed to write RegistrationResponse");
                    }
                }
            }
            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span)
        .await
    }

    async fn handle_disconnect_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        reason: DisconnectReason,
        client_id: String,
        registration_id: Option<String>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.listener.disconnect_request", %client_id, registration_id = ?registration_id, reason = ?reason);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();

        // Use the registration_id from the message if present, otherwise stored one
        let effective_reg = registration_id.or_else(|| state.registration_id.clone());
        if let Some(reg) = effective_reg
            && let Some(manager) = myself.try_get_supervisor()
        {
            let _ = manager.send_message(BrokerMessage::DisconnectRequest {
                reason: reason.clone(),
                client_id: client_id.clone(),
                registration_id: Some(reg),
                trace_ctx: Some(span.context()),
            });
        }

        debug!("Listener received disconnect request, stopping");
        myself.stop(Some(format!("Disconnect: {:?}", reason)));
        Ok(())
    }

    async fn handle_prepare_for_shutdown(
        &self,
        myself: ActorRef<BrokerMessage>,
        _state: &mut ListenerState,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Listener received shutdown notification, stopping");
        myself.stop(Some("SHUTDOWN_GRACEFUL".to_string()));
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_publish_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        topic: String,
        payload: Payload,
        registration_id: String,
        _reply_to: Option<ActorRef<BrokerMessage>>,
        trace_ctx: Option<opentelemetry::Context>,
        published_by: Option<String>,
        id: MessageId,
        window: Hold,
    ) -> Result<(), ActorProcessingErr> {
        let span = debug_span!("cassini.request.publish_request", client_id = %state.client_id, %registration_id, topic = ?topic, payload_bytes = payload.len());
        try_set_parent_otel(&span, trace_ctx.clone());
        async {
            trace!("Listener received publish request");

            let Some(listener_reg) = state.registration_id.clone() else {
                let err_msg = format!("Bad request: no active session for {registration_id:?}");
                warn!("{err_msg}");
                let _ = Listener::write(
                    state.client_id.clone(),
                    ClientMessage::ErrorMessage {
                        error: err_msg,
                        trace_ctx: WireTraceCtx::from_current_span(),
                    },
                    Arc::clone(&state.writer),
                )
                .await;
                return Ok(());
            };

            if registration_id != listener_reg {
                let err_msg = format!("Bad request: session mismatch: {registration_id:?}");
                warn!("{err_msg}");
                let _ = Listener::write(
                    state.client_id.clone(),
                    ClientMessage::ErrorMessage {
                        error: err_msg,
                        trace_ctx: WireTraceCtx::from_current_span(),
                    },
                    Arc::clone(&state.writer),
                )
                .await;
                return Ok(());
            }

            let session = match state.session_ref.as_ref() {
                Some(s) => s.clone(),
                None => {
                    error!(
                        "No session ref for listener {} – dropping publish",
                        state.client_id
                    );
                    return Ok(());
                }
            };

            if let Err(reason) = Self::decide(state, &topic, Access::Publish).await {
                warn!(%topic, identity = ?state.peer_identity, %reason, "publish refused");
                let msg = ClientMessage::PublishResponse {
                    topic,
                    payload: Arc::new(Vec::new()),
                    result: Err(reason),
                    trace_ctx: WireTraceCtx::from_current_span(),
                    published_by: None,
                    origin: Some(id),
                    delivery_id: None,
                    refusal: Some(cassini_types::PublishRefusal::Denied),
                };
                if let Err(e) =
                    Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await
                {
                    error!(error = ?e, "Failed to write a publish refusal");
                }
                return Ok(());
            }
            // The broker keeps the credentials agent's authority as every agent does, believing it only from the
            // identity this connection verified (never from what a client says about itself).
            if BrokerAuthority::carries_authority(&topic)
                && let Ok(floor) = &state.floor
            {
                state.authority.hear(&topic, &payload, floor.identity());
            }

            if let Err(e) = session.send_message(BrokerMessage::PublishRequest {
                registration_id: registration_id.clone(),
                topic: topic.clone(),
                payload,
                reply_to: None,
                trace_ctx,
                published_by,
                id,
                window,
            }) {
                let msg = ClientMessage::PublishResponse {
                    topic,
                    payload: Arc::new(Vec::new()),
                    result: Err(format!("{PUBLISH_REQ_FAILED_TXT}: {e}")),
                    trace_ctx: WireTraceCtx::from_current_span(),
                    published_by: None,
                    origin: Some(id),
                    delivery_id: None,
                    refusal: Some(cassini_types::PublishRefusal::Unavailable),
                };
                if let Err(e) =
                    Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await
                {
                    error!(error = ?e, "Failed to write PublishResponse");
                }
            }
            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span)
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_publish_response(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        topic: String,
        payload: Payload,
        trace_ctx: Option<opentelemetry::Context>,
        result: Result<(), String>,
        published_by: Option<String>,
        origin: Option<MessageId>,
        delivery_id: Option<u64>,
        refusal: Option<cassini_types::PublishRefusal>,
    ) -> Result<(), ActorProcessingErr> {
        let span =
            trace_span!("cassini.listener.publish_response", topic = ?topic, ok = result.is_ok());
        try_set_parent_otel(&span, trace_ctx);
        // A delivery is written only on a topic this connection may still subscribe to: a subscription dropped by
        // a change of authority or a resume can have messages already on their way, and a resumed session is
        // replayed what it held before its subscriptions are judged. Decided (from the cache, usually) before the
        // span is entered, which must not be held across a wait.
        if result.is_ok() {
            let decided = Self::decide(state, &topic, Access::Subscribe)
                .instrument(span.clone())
                .await;
            if let Err(reason) = decided {
                Self::drop_subscription(state, topic, reason)
                    .instrument(span.clone())
                    .await;
                return Ok(());
            }
        }
        // Which subscriber a delivery came from, by its number, for the client's acknowledgement of it.
        if let Some(id) = delivery_id {
            state.deliveries.insert(id, topic.clone());
        }
        async {
            trace!("Listener received publish response");
            let msg = ClientMessage::PublishResponse {
                topic,
                // The payload, and its charge, are held until the write is done.
                payload: payload.bytes().clone(),
                result,
                trace_ctx: WireTraceCtx::from_current_span(),
                published_by,
                origin,
                delivery_id,
                refusal,
            };
            let written =
                Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await;
            if let (Ok(frame), Some(id)) = (&written, delivery_id) {
                state.frame_of.insert(id, *frame);
                state.delivery_in.insert(*frame, id);
            }
            if let Err(e) = written {
                if state.writer.cut.is_cancelled() {
                    // The connection was ended (a slow subscriber's): what waits for it is not written.
                    debug!(error = ?e, "a delivery was not written: the connection was ended");
                } else {
                    error!(error = ?e, "Failed to write PublishResponse");
                }
            }
            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span)
        .await
    }

    /// The client has the delivery numbered `delivery_id`: the subscriber of the topic it came on lets go of it.
    fn handle_delivery_ack(state: &mut ListenerState, delivery_id: u64) {
        if let Some(frame) = state.frame_of.remove(&delivery_id) {
            state.delivery_in.remove(&frame);
        }
        let Some(topic) = state.deliveries.remove(&delivery_id) else {
            debug!(
                delivery_id,
                "an acknowledgement of a delivery this connection did not write"
            );
            return;
        };
        if let Some(registration_id) = &state.registration_id
            && let Some(subscriber) = where_is(crate::get_subscriber_name(registration_id, &topic))
        {
            let _ = subscriber.send_message(BrokerMessage::PublishResponseAck { delivery_id });
        }
    }

    async fn handle_publish_request_ack(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        topic: String,
        id: MessageId,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.listener.publish_ack", topic = ?topic);
        try_set_parent_otel(&span, trace_ctx);
        async {
            let msg = ClientMessage::PublishRequestAck {
                topic,
                epoch: id.epoch,
                seq: id.seq,
                trace_ctx: WireTraceCtx::from_current_span(),
            };
            if let Err(e) =
                Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await
            {
                error!(error = ?e, "Failed to write PublishRequestAck");
            }
            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span)
        .await
    }

    async fn handle_subscribe_ack(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
        result: Result<(), String>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.listener.subscribe_ack", %registration_id, topic = ?topic, ok = result.is_ok());
        try_set_parent_otel(&span, trace_ctx);
        async {
            if result.is_ok() {
                state.dropped.remove(&topic);
                state.subscribed.insert(topic.clone());
            }
            let msg = ClientMessage::SubscribeAcknowledgment {
                topic,
                result,
                trace_ctx: WireTraceCtx::from_current_span(),
            };
            if let Err(e) =
                Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await
            {
                error!(error = ?e, "Failed to write SubscribeAcknowledgment");
            }
            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span)
        .await
    }

    async fn handle_subscribe_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.request.subscribe", client_id = %state.client_id, %registration_id, topic = ?topic);
        try_set_parent_otel(&span, trace_ctx);
        async {
            trace!("Listener received subscribe request");

            let Some(listener_reg) = state.registration_id.clone() else {
                warn!("Subscribe from unregistered client");
                let msg = ClientMessage::SubscribeAcknowledgment {
                    topic,
                    result: Err("Bad request: not registered".to_string()),
                    trace_ctx: WireTraceCtx::from_current_span(),
                };
                let _ =
                    Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await;
                return Ok(());
            };

            if registration_id != listener_reg {
                warn!("Subscribe session mismatch");
                let msg = ClientMessage::SubscribeAcknowledgment {
                    topic,
                    result: Err("Bad request: session mismatch".to_string()),
                    trace_ctx: WireTraceCtx::from_current_span(),
                };
                let _ =
                    Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await;
                return Ok(());
            }

            if let Err(reason) = Self::decide(state, &topic, Access::Subscribe).await {
                warn!(%topic, identity = ?state.peer_identity, %reason, "subscription refused");
                let msg = ClientMessage::SubscribeAcknowledgment {
                    topic,
                    result: Err(reason),
                    trace_ctx: WireTraceCtx::from_current_span(),
                };
                let _ =
                    Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await;
                return Ok(());
            }

            match where_is(listener_reg.clone()) {
                Some(session) => {
                    if let Err(e) = session.send_message(BrokerMessage::SubscribeRequest {
                        registration_id: listener_reg,
                        topic,
                        trace_ctx: Some(span.context()),
                    }) {
                        let err_msg =
                            format!("{SUBSCRIBE_REQUEST_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: {e}");
                        error!("{err_msg}");
                        let _ = Listener::write(
                            state.client_id.clone(),
                            ClientMessage::ErrorMessage {
                                error: err_msg,
                                trace_ctx: WireTraceCtx::from_current_span(),
                            },
                            Arc::clone(&state.writer),
                        )
                        .await;
                    }
                }
                None => {
                    error!("Could not forward subscribe to session; closing connection");
                    let msg = ClientMessage::SubscribeAcknowledgment {
                        topic,
                        result: Err("Failed to complete request: session missing".to_string()),
                        trace_ctx: WireTraceCtx::from_current_span(),
                    };
                    let _ =
                        Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer))
                            .await;
                    myself.stop(Some(SESSION_MISSING_REASON_STR.to_string()));
                }
            }
            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span.clone())
        .await
    }

    async fn handle_unsubscribe_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.request.unsubscribe", client_id = %state.client_id, %registration_id, topic = ?topic);
        try_set_parent_otel(&span, trace_ctx);
        async {
            trace!("Listener received unsubscribe request");

            let Some(listener_reg) = state.registration_id.clone() else {
                warn!("Unsubscribe from unregistered client");
                return Ok(());
            };

            if registration_id != listener_reg {
                warn!("Unsubscribe session mismatch");
                return Ok(());
            }

            match where_is(registration_id.clone()) {
                Some(session) => {
                    if let Err(e) = session.send_message(BrokerMessage::UnsubscribeRequest {
                        registration_id,
                        topic,
                        trace_ctx: Some(span.context()),
                    }) {
                        let err_msg =
                            format!("{SUBSCRIBE_REQUEST_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: {e}");
                        error!("{err_msg}");
                        let _ = Listener::write(
                            state.client_id.clone(),
                            ClientMessage::ErrorMessage {
                                error: err_msg,
                                trace_ctx: WireTraceCtx::from_current_span(),
                            },
                            Arc::clone(&state.writer),
                        )
                        .await;
                    }
                }
                None => {
                    let err_msg =
                        format!("{SUBSCRIBE_REQUEST_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
                    error!("{err_msg}");
                    let _ = Listener::write(
                        state.client_id.clone(),
                        ClientMessage::ErrorMessage {
                            error: err_msg,
                            trace_ctx: WireTraceCtx::from_current_span(),
                        },
                        Arc::clone(&state.writer),
                    )
                    .await;
                }
            }
            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span.clone())
        .await
    }

    async fn handle_unsubscribe_ack(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        topic: String,
        result: Result<(), String>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.listener.unsubscribe_ack", topic = ?topic);
        async {
            debug!(topic = ?topic, "Unsubscribe acknowledged");
            state.subscribed.remove(&topic);
            if state.dropped.contains(&topic) {
                // The broker ended it, and the client was told it is refused.
                return Ok(());
            }
            let msg = ClientMessage::UnsubscribeAcknowledgment {
                topic,
                result,
                trace_ctx: WireTraceCtx::from_current_span(),
            };
            let _ = Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await;
            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span)
        .await
    }

    async fn handle_control_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        registration_id: String,
        op: ControlOp,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.request.control", client_id = %state.client_id, %registration_id, op = ?op);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("Listener received control request");

        let Some(listener_reg) = state.registration_id.clone() else {
            warn!("Control from unregistered client");
            return Ok(());
        };

        if registration_id != listener_reg {
            warn!("Control session mismatch");
            return Ok(());
        }

        if let Some(session) = where_is(listener_reg.clone()) {
            let _ = session.send_message(BrokerMessage::ControlRequest {
                registration_id: listener_reg,
                op,
                reply_to: None,
                trace_ctx: Some(span.context()),
            });
        }
        Ok(())
    }

    async fn handle_control_response(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut ListenerState,
        registration_id: String,
        result: Result<ControlResult, ControlError>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span =
            trace_span!("cassini.listener.control_response", %registration_id, ok = result.is_ok());
        try_set_parent_otel(&span, trace_ctx);
        async {
            trace!("Received ControlResponse");

            let msg = ClientMessage::ControlResponse {
                registration_id,
                result,
                trace_ctx: WireTraceCtx::from_current_span(),
            };

            if let Err(e) =
                Listener::write(state.client_id.clone(), msg, Arc::clone(&state.writer)).await
            {
                error!(error = ?e, "Failed to write ControlResponse");
            }
            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span)
        .await
    }
}
