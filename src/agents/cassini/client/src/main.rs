use anyhow::Result;
use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_client::{init_tracing, shutdown_tracing};
use cassini_types::{ClientEvent, ControlError, ControlOp, ControlResult};
use clap::{Parser, Subcommand, ValueEnum};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

// ===== CLI definition =====

#[derive(Parser)]
#[command(
    name = "cassini",
    about = "CLI client for the Cassini message broker",
    version
)]
struct Cli {
    /// Timeout in seconds for the registration handshake.
    /// Overrides CASSINI_REGISTER_TIMEOUT_SECS env var. Default: 30s.
    #[arg(long, global = true, default_value = "30")]
    register_timeout: u64,

    /// Output format for command results.
    #[arg(long, global = true, default_value = "text", value_enum)]
    format: OutputFormat,

    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Subcommand)]
enum Command {
    /// Publish a message to a topic
    Publish {
        /// Topic to publish to
        topic: String,

        /// Payload as a UTF-8 string
        payload: String,

        /// Timeout in seconds to wait for the publish ack after registration.
        #[arg(long, default_value = "10")]
        publish_timeout: u64,
    },

    /// List all active sessions on the broker
    ListSessions {
        /// Timeout in seconds to wait for the response.
        #[arg(long, default_value = "10")]
        timeout: u64,
    },

    /// List all topics currently registered on the broker
    ListTopics {
        /// Timeout in seconds to wait for the response.
        #[arg(long, default_value = "10")]
        timeout: u64,
    },

    /// Get details for a specific session by registration ID
    GetSession {
        /// The registration ID of the session to retrieve
        registration_id: String,

        /// Timeout in seconds to wait for the response.
        #[arg(long, default_value = "10")]
        timeout: u64,
    },
}

// ===== Output formatting =====

fn print_control_result(result: &ControlResult, format: &OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(result)?);
        }
        OutputFormat::Text => match result {
            ControlResult::SessionList(map) => {
                if map.is_empty() {
                    println!("No active sessions.");
                } else {
                    println!("{} session(s):", map.len());
                    for (id, details) in map {
                        println!("  {id}");
                        println!("    {details:?}");
                    }
                }
            }
            ControlResult::TopicList(topics) => {
                if topics.is_empty() {
                    println!("No topics found.");
                } else {
                    println!("{} topic(s):", topics.len());
                    let mut sorted: Vec<&String> = topics.iter().collect();
                    sorted.sort();
                    for topic in sorted {
                        println!("  {topic}");
                    }
                }
            }
            ControlResult::SessionInfo(details) => {
                println!("{details:?}");
            }
            ControlResult::SubscriberList(subs) => {
                if subs.is_empty() {
                    println!("No subscribers.");
                } else {
                    for sub in subs {
                        println!("  {sub}");
                    }
                }
            }
            ControlResult::Pong => println!("Pong."),
            ControlResult::Disconnected => println!("Disconnected."),
            ControlResult::ShutdownInitiated => println!("Shutdown initiated."),
        },
    }
    Ok(())
}

fn print_control_error(err: &ControlError, format: &OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => {
            eprintln!("{}", serde_json::to_string_pretty(err)?);
        }
        OutputFormat::Text => match err {
            ControlError::NotFound(msg) => eprintln!("Not found: {msg}"),
            ControlError::PermissionDenied(msg) => eprintln!("Permission denied: {msg}"),
            ControlError::InternalError(msg) => eprintln!("Broker internal error: {msg}"),
        },
    }
    Ok(())
}

// ===== Completion event =====

#[derive(Debug)]
enum CompletionEvent {
    Registered(String),
    Published { topic: String },
    ControlResponse(Result<ControlResult, ControlError>),
    TransportError(String),
}

// ===== Bridge actor =====
//
// A single bridge instance lives for the entire connection lifetime. Rather than
// spawning a new bridge per phase and swapping SetEventHandler (which only updates
// state.event_handler but NOT the handler clone captured by the read loop task at
// spawn time), we keep one bridge alive and rearm it between phases via shared
// Arc<Mutex<...>> state that both main and the bridge actor can see.

#[derive(Debug, Clone, PartialEq)]
enum BridgeMode {
    Registration,
    Publish,
    Control,
}

/// Owned by main. Used to rearm the bridge between operation phases without
/// touching the actor system at all.
struct BridgeHandle {
    mode: Arc<Mutex<BridgeMode>>,
    sender: Arc<Mutex<Option<oneshot::Sender<CompletionEvent>>>>,
}

impl BridgeHandle {
    /// Swap in a new mode and a fresh oneshot sender. Returns the receiver to
    /// wait on. Lock ordering is always mode-then-sender to prevent inversion.
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
            // TransportError is always terminal regardless of mode
            (_, ClientEvent::TransportError { reason }) => {
                Some(CompletionEvent::TransportError(reason))
            }
            _ => None,
        };

        if let Some(event) = terminal {
            // Take the sender. If None, we're between phases and this event
            // arrived late (e.g. a stale ack from a previous op) — drop it.
            if let Some(tx) = state.sender.lock().unwrap().take() {
                let _ = tx.send(event);
            }
            // Do NOT stop — this bridge stays alive for the full connection lifetime.
        }

        Ok(())
    }
}

// ===== Session =====

/// Bundles everything needed to drive a live, registered connection.
struct Session {
    client_ref: ActorRef<TcpClientMessage>,
    client_handle: tokio::task::JoinHandle<()>,
    bridge_handle: BridgeHandle,
    _bridge_actor_handle: tokio::task::JoinHandle<()>,
}

impl Session {
    /// Rearm the bridge, send the operation message, and wait for the response.
    /// Does NOT disconnect — call disconnect() explicitly when done.
    async fn send_and_await(
        &self,
        message: TcpClientMessage,
        mode: BridgeMode,
        op_timeout: Duration,
        timeout_msg: &str,
    ) -> Result<CompletionEvent> {
        let rx = self.bridge_handle.rearm(mode);

        self.client_ref
            .send_message(message)
            .map_err(|e| anyhow::anyhow!("Failed to send message to client actor: {e}"))?;

        wait_for_event(rx, op_timeout, timeout_msg).await
    }

    /// Send a clean disconnect to the broker and await full actor shutdown.
    async fn disconnect(self) {
        self.client_ref
            .send_message(TcpClientMessage::Disconnect { trace_ctx: None })
            .ok();
        let _ = self.client_handle.await;
        // _bridge_actor_handle drops here, stopping the bridge task cleanly
    }
}

// ===== Helpers =====

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

/// Spawn the bridge and TCP client, block until registration is acked, and
/// return a live Session. The bridge is armed for Registration before anything
/// is spawned so no events can be missed.
async fn register(register_timeout: Duration) -> Result<Session> {
    let config = TCPClientConfig::new()
        .map_err(|e| anyhow::anyhow!("Failed to load config from environment: {e}"))?;

    // Build shared state and arm for registration before spawning anything.
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
            config,
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
            eprintln!("Registered. registration_id={id}");
            Ok(Session {
                client_ref,
                client_handle,
                bridge_handle,
                _bridge_actor_handle: bridge_actor_handle,
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

// ===== Command handlers =====

async fn run_publish(
    topic: String,
    payload: String,
    register_timeout: Duration,
    publish_timeout: Duration,
    format: &OutputFormat,
) -> Result<()> {
    let session = register(register_timeout).await?;

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
            OutputFormat::Json => println!("{{\"status\":\"ok\",\"topic\":\"{topic}\"}}"),
            OutputFormat::Text => println!("Published to topic={topic}"),
        },
        CompletionEvent::TransportError(reason) => {
            return Err(anyhow::anyhow!("Transport error during publish: {reason}"));
        }
        other => {
            return Err(anyhow::anyhow!(
                "Unexpected event during publish: {other:?}"
            ));
        }
    }

    Ok(())
}

async fn run_control(
    message: TcpClientMessage,
    register_timeout: Duration,
    op_timeout: Duration,
    timeout_msg: &str,
    format: &OutputFormat,
) -> Result<()> {
    let session = register(register_timeout).await?;

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
            return Err(anyhow::anyhow!("Transport error: {reason}"));
        }
        other => return Err(anyhow::anyhow!("Unexpected event: {other:?}")),
    }

    Ok(())
}

// ===== Entry point =====

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("cassini-cli");

    let cli = Cli::parse();
    let register_timeout = Duration::from_secs(cli.register_timeout);
    let format = &cli.format;

    let result = match cli.command {
        Command::Publish {
            topic,
            payload,
            publish_timeout,
        } => {
            run_publish(
                topic,
                payload,
                register_timeout,
                Duration::from_secs(publish_timeout),
                format,
            )
            .await
        }

        Command::ListSessions { timeout } => {
            run_control(
                TcpClientMessage::ListSessions { trace_ctx: None },
                register_timeout,
                Duration::from_secs(timeout),
                "Timed out waiting for session list",
                format,
            )
            .await
        }

        Command::ListTopics { timeout } => {
            run_control(
                TcpClientMessage::ListTopics { trace_ctx: None },
                register_timeout,
                Duration::from_secs(timeout),
                "Timed out waiting for topic list",
                format,
            )
            .await
        }

        Command::GetSession {
            registration_id,
            timeout,
        } => {
            run_control(
                TcpClientMessage::ControlRequest {
                    op: ControlOp::GetSessionInfo { registration_id },
                    trace_ctx: None,
                },
                register_timeout,
                Duration::from_secs(timeout),
                "Timed out waiting for session details",
                format,
            )
            .await
        }
    };

    if let Err(e) = &result {
        eprintln!("Error: {e}");
        shutdown_tracing();
        std::process::exit(1);
    }

    shutdown_tracing();
    result
}
