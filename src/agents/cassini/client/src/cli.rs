use crate::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use anyhow::Result;
use cassini_types::{ClientEvent, ControlError, ControlOp, ControlResult};
use clap::{Parser, Subcommand, ValueEnum};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;
use tracing::{error, info};
use std::io::{BufRead, BufReader};
use std::fs::OpenOptions;
use crate::QueueEntry;
// ===== CLI definition =====

/// Default socket path if XDG_RUNTIME_DIR is not set.
pub const DEFAULT_SOCK_PATH: &str = "/tmp/cassini-daemon.sock";
pub const DEFAULT_PID_PATH: &str = "/tmp/cassini-daemon.pid";
pub const DEFAULT_QUEUE_PATH: &str = "/tmp/cassini-queue.jsonl";

#[derive(Parser)]
#[command(
    name = "cassini",
    about = "CLI client for the Cassini message broker",
    version
)]
pub struct Cli {
    /// Timeout in seconds for the registration handshake.
    #[arg(long, global = true, default_value = "30")]
    pub register_timeout: u64,

    /// Output format for command results.
    #[arg(long, global = true, default_value = "text", value_enum)]
    pub format: OutputFormat,

    /// Run as a persistent daemon, listening on a Unix socket for commands.
    /// Subsequent invocations without --daemon will use the socket automatically
    /// if it is reachable. If the socket is specified but unreachable, the
    /// command fails rather than silently falling back to direct connect.
    #[arg(long, short = 'd', global = false)]
    pub daemon: bool,

    /// Keep the daemon in the foreground instead of forking. Useful for systemd.
    #[arg(long, requires = "daemon")]
    pub foreground: bool,

    /// Override the Unix socket path used for daemon IPC.
    /// Overrides CASSINI_DAEMON_SOCK env var and the default path.
    #[arg(long, global = true)]
    pub socket: Option<PathBuf>,

    /// Path to the persistent message queue file.
    /// Messages are stored here when the broker is unavailable.
    #[arg(long, global = true, default_value = DEFAULT_QUEUE_PATH)]
    pub queue: PathBuf,

    /// Override the broker address (overrides BROKER_ADDR env var).
    #[arg(long, global = false)]
    pub broker_addr: Option<String>,

    /// Override the CA certificate path (overrides TLS_CA_CERT env var).
    #[arg(long, global = false)]
    pub ca_cert: Option<String>,

    /// Override the client certificate path (overrides TLS_CLIENT_CERT env var).
    #[arg(long, global = false)]
    pub client_cert: Option<String>,

    /// Override the client key path (overrides TLS_CLIENT_KEY env var).
    #[arg(long, global = false)]
    pub client_key: Option<String>,

    /// Override the server name (overrides CASSINI_SERVER_NAME env var).
    #[arg(long, global = false)]
    pub server_name: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Subcommand)]
pub enum Command {
    /// Publish a message to a topic
    Publish {
        topic: String,
        payload: String,
        #[arg(long, default_value = "10")]
        publish_timeout: u64,
    },

    /// List all active sessions on the broker
    ListSessions {
        #[arg(long, default_value = "10")]
        timeout: u64,
    },

    /// List all topics currently registered on the broker
    ListTopics {
        #[arg(long, default_value = "10")]
        timeout: u64,
    },

    /// Get details for a specific session by registration ID
    GetSession {
        registration_id: String,
        #[arg(long, default_value = "10")]
        timeout: u64,
    },

    /// Print the daemon's current status (pid, socket path, registration id)
    Status,

    /// Replay queued messages from the default queue file to the broker.
    Drain,

    /// Replay queued messages from a specific queue file to the broker.
    Replay {
        #[arg(long, default_value = DEFAULT_QUEUE_PATH)]
        queue: PathBuf,
    },
}

// ===== IPC protocol =====
//
// Framing: [u32 BE length][JSON body]
// Both request and response use this framing.
// The daemon serializes all operations — no concurrent broker requests.

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum IpcRequest {
    Publish {
        topic: String,
        /// Base64-encoded payload so arbitrary bytes survive JSON.
        payload_b64: String,
        timeout_secs: u64,
    },
    ListSessions {
        timeout_secs: u64,
    },
    ListTopics {
        timeout_secs: u64,
    },
    GetSession {
        registration_id: String,
        timeout_secs: u64,
    },
    Status,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IpcResponse {
    Ok {
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<serde_json::Value>,
    },
    Error {
        reason: String,
    },
}

async fn ipc_write(stream: &mut UnixStream, response: &IpcResponse) -> Result<()> {
    let bytes = serde_json::to_vec(response)?;
    let len = bytes.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&bytes).await?;
    Ok(())
}

async fn ipc_read_request(stream: &mut UnixStream) -> Result<IpcRequest> {
    let len = stream.read_u32().await? as usize;
    if len > 4 * 1024 * 1024 {
        anyhow::bail!("IPC request too large: {len} bytes");
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

async fn ipc_write_request(stream: &mut UnixStream, request: &IpcRequest) -> Result<()> {
    let bytes = serde_json::to_vec(request)?;
    let len = bytes.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&bytes).await?;
    Ok(())
}

pub async fn ipc_read_response(stream: &mut UnixStream) -> Result<IpcResponse> {
    let len = stream.read_u32().await? as usize;
    if len > 4 * 1024 * 1024 {
        anyhow::bail!("IPC response too large: {len} bytes");
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

// ===== Output formatting =====

pub fn print_control_result(result: &ControlResult, format: &OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => {
            info!("{}", serde_json::to_string_pretty(result)?);
        }
        OutputFormat::Text => match result {
            ControlResult::SessionList(map) => {
                if map.is_empty() {
                    info!("No active sessions.");
                } else {
                    info!("{} session(s):", map.len());
                    for (id, details) in map {
                        info!("  {id}");
                        info!("    {details:?}");
                    }
                }
            }
            ControlResult::TopicList(topics) => {
                if topics.is_empty() {
                    info!("No topics found.");
                } else {
                    info!("{} topic(s):", topics.len());
                    let mut sorted: Vec<&String> = topics.iter().collect();
                    sorted.sort();
                    for topic in sorted {
                        info!("  {topic}");
                    }
                }
            }
            ControlResult::SessionInfo(details) => {
                info!("{details:?}");
            }
            ControlResult::SubscriberList(subs) => {
                if subs.is_empty() {
                    info!("No subscribers.");
                } else {
                    for sub in subs {
                        info!("  {sub}");
                    }
                }
            }
            ControlResult::Pong => info!("Pong."),
            ControlResult::Disconnected => info!("Disconnected."),
            ControlResult::ShutdownInitiated => info!("Shutdown initiated."),
        },
    }
    Ok(())
}

fn print_control_error(err: &ControlError, format: &OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => {
            error!("{}", serde_json::to_string_pretty(err)?);
        }
        OutputFormat::Text => match err {
            ControlError::NotFound(msg) => error!("Not found: {msg}"),
            ControlError::PermissionDenied(msg) => error!("Permission denied: {msg}"),
            ControlError::InternalError(msg) => error!("Broker internal error: {msg}"),
        },
    }
    Ok(())
}

fn format_ipc_response(response: IpcResponse, format: &OutputFormat) -> Result<()> {
    match response {
        IpcResponse::Ok { result: Some(val) } => match format {
            OutputFormat::Json => info!("{}", serde_json::to_string_pretty(&val)?),
            OutputFormat::Text => {
                // The daemon sends pre-serialized ControlResult / publish ack.
                // Deserialize and print using the same text formatter.
                if let Ok(ctrl) = serde_json::from_value::<ControlResult>(val.clone()) {
                    print_control_result(&ctrl, format)?;
                } else {
                    // Publish ack or other simple response — just print the value.
                    info!("{val}");
                }
            }
        },
        IpcResponse::Ok { result: None } => match format {
            OutputFormat::Json => info!("{{\"status\":\"ok\"}}"),
            OutputFormat::Text => info!("ok"),
        },
        IpcResponse::Error { reason } => {
            anyhow::bail!("{reason}");
        }
    }
    Ok(())
}

// ===== Completion event =====

#[derive(Debug)]
pub enum CompletionEvent {
    Registered(String),
    Published { topic: String },
    ControlResponse(Result<ControlResult, ControlError>),
    TransportError(String),
}

// ===== Bridge actor =====

#[derive(Debug, Clone, PartialEq)]
pub enum BridgeMode {
    Registration,
    Publish,
    Control,
}

struct BridgeHandle {
    mode: Arc<Mutex<BridgeMode>>,
    sender: Arc<Mutex<Option<oneshot::Sender<CompletionEvent>>>>,
}

impl BridgeHandle {
    fn rearm(&self, new_mode: BridgeMode) -> oneshot::Receiver<CompletionEvent> {
        let (tx, rx) = oneshot::channel();
        *self.mode.lock().unwrap() = new_mode;
        *self.sender.lock().unwrap() = Some(tx);
        rx
    }
}

struct CompletionBridge;

struct CompletionBridgeState {
    mode: Arc<Mutex<BridgeMode>>,
    sender: Arc<Mutex<Option<oneshot::Sender<CompletionEvent>>>>,
}

struct CompletionBridgeArgs {
    mode: Arc<Mutex<BridgeMode>>,
    sender: Arc<Mutex<Option<oneshot::Sender<CompletionEvent>>>>,
}

#[async_trait]
impl Actor for CompletionBridge {
    type Msg = ClientEvent;
    type State = CompletionBridgeState;
    type Arguments = CompletionBridgeArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(CompletionBridgeState {
            mode: args.mode,
            sender: args.sender,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let mode = state.mode.lock().unwrap().clone();

        let terminal: Option<CompletionEvent> = match (mode, msg) {
            (BridgeMode::Registration, ClientEvent::Registered { registration_id }) => {
                Some(CompletionEvent::Registered(registration_id))
            }
            (BridgeMode::Publish, ClientEvent::PublishAcknowledged { topic }) => {
                Some(CompletionEvent::Published { topic })
            }
            (BridgeMode::Control, ClientEvent::ControlResponse { result, .. }) => {
                Some(CompletionEvent::ControlResponse(result))
            }
            (_, ClientEvent::TransportError { reason }) => {
                Some(CompletionEvent::TransportError(reason))
            }
            _ => None,
        };

        if let Some(event) = terminal {
            if let Some(tx) = state.sender.lock().unwrap().take() {
                let _ = tx.send(event);
            }
        }

        Ok(())
    }
}

// ===== Session =====

pub struct Session {
    client_ref: ActorRef<TcpClientMessage>,
    client_handle: tokio::task::JoinHandle<()>,
    bridge_handle: BridgeHandle,
    _bridge_actor_handle: tokio::task::JoinHandle<()>,
    registration_id: String,
}

impl Session {
    pub fn get_id(&self) -> String {
        self.registration_id.clone()
    }

    pub async fn send_and_await(
        &self,
        message: TcpClientMessage,
        mode: BridgeMode,
        op_timeout: Duration,
        timeout_msg: &str,
    ) -> Result<CompletionEvent> {
        let rx = self.bridge_handle.rearm(mode);
        self.client_ref
            .send_message(message)
            .map_err(|e| anyhow::anyhow!("Failed to send message: {e}"))?;
        wait_for_event(rx, op_timeout, timeout_msg).await
    }

    pub async fn disconnect(self) {
        self.client_ref
            .send_message(TcpClientMessage::Disconnect { trace_ctx: None })
            .ok();
        let _ = self.client_handle.await;
    }
}

/// Returns true if the socket path exists AND a connection succeeds.
/// A file that exists but refuses connections is a stale socket.
pub async fn daemon_is_reachable(socket_path: &PathBuf) -> bool {
    if !socket_path.exists() {
        return false;
    }
    UnixStream::connect(socket_path).await.is_ok()
}

async fn wait_for_event(
    rx: oneshot::Receiver<CompletionEvent>,
    timeout: Duration,
    timeout_msg: &str,
) -> Result<CompletionEvent> {
    tokio::select! {
        res = rx => {
            res.map_err(|_| anyhow::anyhow!("Bridge actor dropped sender unexpectedly"))
        }
        _ = tokio::time::sleep(timeout) => {
            Err(anyhow::anyhow!("{} ({}s)", timeout_msg, timeout.as_secs()))
        }
    }
}

pub async fn append_to_queue(queue_path: &PathBuf, entry: &QueueEntry) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(queue_path)?;
    let line = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    writeln!(file, "{}", line)
}

pub async fn drain_queue(
    queue_path: PathBuf,
    client_config: TCPClientConfig,
    register_timeout: Duration,
) -> Result<()> {
    if !queue_path.exists() {
        info!("Queue file does not exist, nothing to drain.");
        return Ok(());
    }

    let file = std::fs::File::open(&queue_path)?;
    let reader = BufReader::new(file);
    let entries: Vec<QueueEntry> = reader
        .lines()
        .filter_map(|l| l.ok().and_then(|s| serde_json::from_str(&s).ok()))
        .collect();

    if entries.is_empty() {
        info!("Queue is empty, nothing to drain.");
        return Ok(());
    }

    info!("Draining {} queued message(s) to broker...", entries.len());

    let session = register(client_config, register_timeout).await?;

    for entry in &entries {
        let payload = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            &entry.payload_b64,
        )
        .map_err(|e| anyhow::anyhow!("Failed to decode payload: {e}"))?;

        session
            .send_and_await(
                TcpClientMessage::Publish {
                    topic: entry.topic.clone(),
                    payload,
                    trace_ctx: None,
                },
                BridgeMode::Publish,
                Duration::from_secs(10),
                "Timed out waiting for publish ack",
            )
            .await?;

        info!("Drained: {}", entry.topic);
    }

    session.disconnect().await;
    std::fs::remove_file(&queue_path)?;
    info!("Queue drained and cleared.");
    Ok(())
}

pub async fn register(
    client_config: TCPClientConfig,
    register_timeout: Duration,
) -> Result<Session> {
    let mode = Arc::new(Mutex::new(BridgeMode::Registration));
    let sender_slot: Arc<Mutex<Option<oneshot::Sender<CompletionEvent>>>> =
        Arc::new(Mutex::new(None));
    let (tx, reg_rx) = oneshot::channel();
    *sender_slot.lock().unwrap() = Some(tx);

    let bridge_handle = BridgeHandle {
        mode: Arc::clone(&mode),
        sender: Arc::clone(&sender_slot),
    };

    let (bridge_ref, bridge_actor_handle) = Actor::spawn(
        Some("cassini.cli.completion_bridge".to_string()),
        CompletionBridge,
        CompletionBridgeArgs {
            mode,
            sender: sender_slot,
        },
    )
    .await?;

    let (client_ref, client_handle) = Actor::spawn(
        Some("cassini.cli.tcp_client".to_string()),
        TcpClientActor,
        TcpClientArgs {
            config: client_config,
            registration_id: None,
            events_output: None,
            event_handler: Some(bridge_ref),
        },
    )
    .await?;

    match wait_for_event(
        reg_rx,
        register_timeout,
        "Timed out waiting for registration ack",
    )
    .await?
    {
        CompletionEvent::Registered(id) => {
            info!("Registered. registration_id={id}");
            Ok(Session {
                client_ref,
                client_handle,
                bridge_handle,
                _bridge_actor_handle: bridge_actor_handle,
                registration_id: id,
            })
        }
        CompletionEvent::TransportError(reason) => {
            client_ref.stop(None);
            let _ = client_handle.await;
            Err(anyhow::anyhow!(
                "Transport error during registration: {reason}"
            ))
        }
        other => {
            client_ref.stop(None);
            let _ = client_handle.await;
            Err(anyhow::anyhow!(
                "Unexpected event during registration: {other:?}"
            ))
        }
    }
}

// ===== Daemon =====

/// Handles a single IPC connection from a CLI client. Acquires the session
/// mutex, executes the operation, writes the response, releases the mutex.
/// This serializes all broker operations — the broker session is not
/// multiplexable and responses carry no correlation id.
async fn handle_ipc_connection(
    mut stream: UnixStream,
    session: Arc<tokio::sync::Mutex<Option<Session>>>,
    queue_path: PathBuf,
) {
    let request = match ipc_read_request(&mut stream).await {
        Ok(r) => r,
        Err(e) => {
            let _ = ipc_write(
                &mut stream,
                &IpcResponse::Error {
                    reason: format!("Failed to read request: {e}"),
                },
            )
            .await;
            return;
        }
    };

    let response = match request {
        IpcRequest::Status => {
            let session = session.lock().await;
            let registration_id = session
                .as_ref()
                .map(|s| s.registration_id.clone())
                .unwrap_or_else(|| "unregistered".to_string());
            IpcResponse::Ok {
                result: Some(serde_json::json!({
                    "pid": std::process::id(),
                    "registration_id": registration_id,
                })),
            }
        }

        IpcRequest::Publish {
            topic,
            payload_b64,
            timeout_secs: _,
        } => {
            // Publish is always queued if broker unavailable — fire and forget.
            let session = session.lock().await;
            if let Some(ref s) = *session {
                // Broker available — send directly.
                let payload = match base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    &payload_b64,
                ) {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = ipc_write(
                            &mut stream,
                            &IpcResponse::Error {
                                reason: format!("Invalid base64 payload: {e}"),
                            },
                        )
                        .await;
                        return;
                    }
                };
                match s.send_and_await(
                    TcpClientMessage::Publish {
                        topic: topic.clone(),
                        payload,
                        trace_ctx: None,
                    },
                    BridgeMode::Publish,
                    Duration::from_secs(10),
                    "Timed out waiting for publish ack",
                )
                .await
                {
                    Ok(CompletionEvent::Published { .. }) => IpcResponse::Ok { result: None },
                    Ok(CompletionEvent::TransportError(r)) => {
                        // Transport error — fall through to queue.
                        let entry = QueueEntry {
                            topic,
                            payload_b64,
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            attempts: 0,
                        };
                        if let Err(e) = append_to_queue(&queue_path, &entry).await {
                            IpcResponse::Error { reason: format!("Transport error and queue failed: {r} / {e}") }
                        } else {
                            IpcResponse::Ok { result: None }
                        }
                    }
                    Ok(other) => IpcResponse::Error {
                        reason: format!("Unexpected event: {other:?}"),
                    },
                    Err(e) => IpcResponse::Error {
                        reason: e.to_string(),
                    },
                }
            } else {
                // No broker session — queue the message.
                let entry = QueueEntry {
                    topic,
                    payload_b64,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    attempts: 0,
                };
                match append_to_queue(&queue_path, &entry).await {
                    Ok(_) => IpcResponse::Ok { result: None },
                    Err(e) => IpcResponse::Error {
                        reason: format!("Broker unavailable and queue failed: {e}"),
                    },
                }
            }
        }

        // Control operations require a live broker — return error if unavailable.
        IpcRequest::ListSessions { timeout_secs } => {
            let session = session.lock().await;
            match session.as_ref() {
                Some(s) => handle_control_ipc(s, TcpClientMessage::ListSessions { trace_ctx: None }, timeout_secs, "Timed out waiting for session list").await,
                None => IpcResponse::Error { reason: "Broker unavailable".to_string() },
            }
        }

        IpcRequest::ListTopics { timeout_secs } => {
            let session = session.lock().await;
            match session.as_ref() {
                Some(s) => handle_control_ipc(s, TcpClientMessage::ListTopics { trace_ctx: None }, timeout_secs, "Timed out waiting for topic list").await,
                None => IpcResponse::Error { reason: "Broker unavailable".to_string() },
            }
        }

        IpcRequest::GetSession { registration_id, timeout_secs } => {
            let session = session.lock().await;
            match session.as_ref() {
                Some(s) => handle_control_ipc(
                    s,
                    TcpClientMessage::ControlRequest {
                        op: ControlOp::GetSessionInfo { registration_id },
                        trace_ctx: None,
                    },
                    timeout_secs,
                    "Timed out waiting for session details",
                ).await,
                None => IpcResponse::Error { reason: "Broker unavailable".to_string() },
            }
        }
    };

    let _ = ipc_write(&mut stream, &response).await;
}

async fn handle_control_ipc(
    session: &Session,
    message: TcpClientMessage,
    timeout_secs: u64,
    timeout_msg: &str,
) -> IpcResponse {
    match session
        .send_and_await(
            message,
            BridgeMode::Control,
            Duration::from_secs(timeout_secs),
            timeout_msg,
        )
        .await
    {
        Ok(CompletionEvent::ControlResponse(Ok(result))) => match serde_json::to_value(&result) {
            Ok(val) => IpcResponse::Ok { result: Some(val) },
            Err(e) => IpcResponse::Error {
                reason: format!("Serialization error: {e}"),
            },
        },
        Ok(CompletionEvent::ControlResponse(Err(e))) => IpcResponse::Error {
            reason: format!("{e:?}"),
        },
        Ok(CompletionEvent::TransportError(r)) => IpcResponse::Error {
            reason: format!("Transport error: {r}"),
        },
        Ok(other) => IpcResponse::Error {
            reason: format!("Unexpected event: {other:?}"),
        },
        Err(e) => IpcResponse::Error {
            reason: e.to_string(),
        },
    }
}

fn resolve_pid_path(socket_path: &PathBuf) -> PathBuf {
    socket_path.with_extension("pid")
}

pub async fn run_daemon(
    socket_path: PathBuf,
    client_config: Option<TCPClientConfig>,
    register_timeout: Duration,
    foreground: bool,
    queue_path: PathBuf,
) -> Result<()> {
    // Clean up any stale socket from a previous crash.
    if socket_path.exists() {
        if daemon_is_reachable(&socket_path).await {
            anyhow::bail!(
                "Daemon already running at {}. Use `cassini status` to inspect it.",
                socket_path.display()
            );
        }
        error!("Removing stale socket at {}", socket_path.display());
        std::fs::remove_file(&socket_path)?;
    }

    // Ensure parent directory exists.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if !foreground {
        // Safety: fork() in a tokio runtime is unsound. We exec ourselves with
        // --foreground instead, which is the correct daemonization approach when
        // the process is already async. The parent exits immediately after spawning.
        let exe = std::env::current_exe()?;
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut child_args: Vec<String> = args
            .into_iter()
            .filter(|a| a != "--daemon" && a != "-d")
            .collect();
        child_args.push("--foreground".to_string());

        std::process::Command::new(exe)
            .args(&child_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn daemon process: {e}"))?;

        info!("Daemon started. Socket: {}", socket_path.display());
        return Ok(());
    }

    // Foreground path — we are the daemon.
    let pid_path = resolve_pid_path(&socket_path);
    std::fs::write(&pid_path, std::process::id().to_string())?;

    // Bind socket FIRST — daemon is reachable immediately.
    // Broker connection happens in the background.
    let listener = UnixListener::bind(&socket_path)?;
    info!("Daemon listening on {}", socket_path.display());

    let session: Arc<tokio::sync::Mutex<Option<Session>>> = Arc::new(tokio::sync::Mutex::new(None));

    // Attempt broker connection in background if config is available.
    if let Some(config) = client_config {
        let session_clone = Arc::clone(&session);
        tokio::spawn(async move {
            match register(config, register_timeout).await {
                Ok(s) => {
                    info!("Broker connection established. registration_id={}", s.registration_id);
                    *session_clone.lock().await = Some(s);
                }
                Err(e) => {
                    error!("Failed to connect to broker: {e} — daemon will queue messages");
                }
            }
        });
    } else {
        info!("No broker config available — daemon will queue all messages");
    }

    // Signal handler — clean disconnect on SIGTERM/SIGINT.
    let socket_path_clone = socket_path.clone();
    let pid_path_clone = pid_path.clone();
    tokio::spawn(async move {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler");
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .expect("Failed to install SIGINT handler");
        tokio::select! {
            _ = sigterm.recv() => error!("Received SIGTERM, shutting down"),
            _ = sigint.recv() => info!("Received SIGINT, shutting down"),
        }
        let _ = std::fs::remove_file(&socket_path_clone);
        let _ = std::fs::remove_file(&pid_path_clone);
        std::process::exit(0);
    });

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let session_clone = Arc::clone(&session);
                let queue_path_clone = queue_path.clone();
                tokio::spawn(async move {
                    handle_ipc_connection(stream, session_clone, queue_path_clone).await;
                });
            }
            Err(e) => {
                error!("Accept error: {e}");
            }
        }
    }
}

// ===== Client-side IPC dispatch =====

/// Send a request to the running daemon and print the response.
pub async fn dispatch_to_daemon(
    socket_path: &PathBuf,
    request: IpcRequest,
    format: &OutputFormat,
) -> Result<()> {
    let mut stream = UnixStream::connect(socket_path).await.map_err(|e| {
        anyhow::anyhow!(
            "Daemon socket at {} is unreachable: {e}\n\
             Start the daemon with `cassini --daemon` or remove --socket / CASSINI_DAEMON_SOCK \
             to use direct connect.",
            socket_path.display()
        )
    })?;

    ipc_write_request(&mut stream, &request).await?;
    let response = ipc_read_response(&mut stream).await?;
    format_ipc_response(response, format)
}

// ===== Command handlers (direct connect path) =====

pub async fn run_publish_direct(
    client_config: TCPClientConfig,
    topic: String,
    payload: String,
    register_timeout: Duration,
    publish_timeout: Duration,
    format: &OutputFormat,
) -> Result<()> {
    let session = register(client_config, register_timeout).await?;

    let result = session
        .send_and_await(
            TcpClientMessage::Publish {
                topic,
                payload: payload.into_bytes(),
                trace_ctx: None,
            },
            BridgeMode::Publish,
            publish_timeout,
            "Timed out waiting for publish ack",
        )
        .await;

    session.disconnect().await;

    match result? {
        CompletionEvent::Published { topic } => match format {
            OutputFormat::Json => info!("{{\"status\":\"ok\",\"topic\":\"{topic}\"}}"),
            OutputFormat::Text => info!("Published to topic={topic}"),
        },
        CompletionEvent::TransportError(reason) => {
            anyhow::bail!("Transport error during publish: {reason}")
        }
        other => anyhow::bail!("Unexpected event during publish: {other:?}"),
    }

    Ok(())
}

pub async fn run_control_direct(
    client_config: TCPClientConfig,
    message: TcpClientMessage,
    register_timeout: Duration,
    op_timeout: Duration,
    timeout_msg: &str,
    format: &OutputFormat,
) -> Result<()> {
    let session = register(client_config, register_timeout).await?;

    let result = session
        .send_and_await(message, BridgeMode::Control, op_timeout, timeout_msg)
        .await;

    session.disconnect().await;

    match result? {
        CompletionEvent::ControlResponse(Ok(result)) => {
            print_control_result(&result, format)?;
        }
        CompletionEvent::ControlResponse(Err(err)) => {
            print_control_error(&err, format)?;
            std::process::exit(1);
        }
        CompletionEvent::TransportError(reason) => {
            anyhow::bail!("Transport error: {reason}")
        }
        other => anyhow::bail!("Unexpected event: {other:?}"),
    }

    Ok(())
}
