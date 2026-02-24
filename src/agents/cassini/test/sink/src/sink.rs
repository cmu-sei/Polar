use cassini_client::{
    ClientEventForwarder, ClientEventForwarderArgs, TCPClientConfig, TcpClientActor,
    TcpClientArgs, TcpClientMessage, try_set_parent_wire,
};

use harness_common::{Envelope, validate_checksum, SupervisorMessage};
use ractor::{
    Actor, ActorProcessingErr, ActorRef, async_trait,
    concurrency::{Duration, Instant},
};
use rkyv::rancor;
use serde_json::to_string_pretty;

use std::path::Path;
use tokio::fs;
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

use tracing::{debug, error, info, trace, trace_span};
use cassini_types::{ClientEvent, WireTraceCtx};

// ============================== Sink Actor Definition ============================== //
//

// Metrics tracked by the sink
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SinkMetrics {
    /// messages received
    pub received: usize,
    /// time since last message
    pub last_interarrival: Option<Duration>,
    /// minumum time between messages
    pub min_interarrival: Option<Duration>,
    /// longest time between messages
    pub max_interarrival: Option<Duration>,
    /// average time between messages over the course of a given test run
    pub avg_interarrival: Option<f64>,
}

pub struct SinkAgent;

#[derive(Clone)]
pub struct SinkConfig {
    pub topic: String,
    pub expected_count: Option<usize>, // None = run until explicitly stopped
}

pub struct SinkState {
    cfg: SinkConfig,
    metrics: SinkMetrics,
    is_stopping: bool,
    expected_count: Option<usize>,
    last_seen: Option<Instant>,
    tcp_client: ActorRef<TcpClientMessage>,
    instance_id: String,
}

pub enum SinkAgentMsg {
    Start,
    Receive { 
        payload: Vec<u8>, 
        trace_ctx: Option<WireTraceCtx>,
    },
    Error {
        reason: String,
    },
    GracefulStop,
    // SetExpectedCount(usize),
}

impl SinkAgent {
    /// Append a JSON value to a file as a single line (NDJSON format).
    pub async fn append_json_to_file<P: AsRef<Path>>(
        path: P,
        value: &Envelope,
    ) -> tokio::io::Result<()> {
        // Open or create file in append mode
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;

        // Serialize to compact JSON string – map serialization error to io::Error
        let json_str = serde_json::to_string(value)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        // Write with trailing newline
        file.write_all(json_str.as_bytes()).await?;
        file.write_all(b"\n").await?;
        file.flush().await?;

        Ok(())
    }

    pub fn validate_checksums(path: &str) -> std::io::Result<()> {
        use std::fs::File;
        use std::io::{BufRead, BufReader};

        let file = File::open(path)?;
        let reader = BufReader::new(file);

        for (i, line) in reader.lines().enumerate() {
            let line = line?;
            match serde_json::from_str::<Envelope>(&line) {
                Ok(message) => {
                    if !validate_checksum(message.data.as_bytes(), &message.checksum) {
                        error!("Failed to validate checksum for message: {}", message.seqno);
                    }
                }
                Err(e) => {
                    error!("Failed to parse JSON at line {}: {}", i + 1, e);
                    // Log the bad line for debugging
                    error!("Problem line: {}", &line[..line.len().min(200)]);
                    // Continue instead of panicking
                }
            }
        }

        Ok(())
    }
}

#[async_trait]
impl Actor for SinkAgent {
    type Msg = SinkAgentMsg;
    type State = SinkState;
    type Arguments = SinkConfig;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: SinkConfig,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let actor_name = myself.get_name().unwrap_or_default();
        let instance_id = actor_name.split('.').last().unwrap_or("unknown").to_string();
        info!("Sink actor {} starting with instance_id={}", actor_name, instance_id);

        // Remove any leftover output file from previous runs
        let file_path = format!("{}-output.jsonl", args.topic);
        let _ = fs::remove_file(&file_path).await;

        // Create an output port (unused, but required by TCP client API)
        // ---------- 1. Spawn the bridge first ----------
        let (forwarder, _) = Actor::spawn_linked(
            Some(format!("sink.{}.{}.event-bridge", args.topic, instance_id)),
            ClientEventForwarder::new(),
            ClientEventForwarderArgs {
                target: myself.clone(),
                mapper: Box::new(|event| match event {
                    ClientEvent::Registered { .. } => Some(SinkAgentMsg::Start),
                    ClientEvent::MessagePublished { payload, trace_ctx, .. } => {
                        Some(SinkAgentMsg::Receive { payload, trace_ctx })
                    }
                    ClientEvent::TransportError { reason } => Some(SinkAgentMsg::Error { reason }),
                    _ => None,
                }),
            },
            myself.clone().into(),
            )
            .await?;

        // ---------- 2. TCP client config ----------
        let tcp_cfg = match TCPClientConfig::new() {
            Ok(cfg) => cfg,
            Err(e) => {
                error!("Failed to create TCP client config: {}", e);
                return Err(e.into());
            }
        };

        // ---------- 3. Spawn TCP client with the bridge as the event handler ----------
        let (client, _) = Actor::spawn_linked(
            Some(format!("sink.{}.{}.tcp-client", args.topic, instance_id)),
            TcpClientActor,
            TcpClientArgs {
                config: tcp_cfg,
                registration_id: None,
                events_output: None,
                event_handler: Some(forwarder.into()),
            },
            myself.clone().into(),
        )
        .await
        .map_err(|e| {
            error!("Failed to spawn TCP client actor: {}", e);
            e
        })?;

        Ok(SinkState {
            cfg: args.clone(),
            metrics: SinkMetrics::default(),
            is_stopping: false,
            expected_count: args.expected_count,
            tcp_client: client,
            last_seen: None,
            instance_id,
        })
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("[{}] Sink actor stopping, final received count = {}", state.instance_id, state.metrics.received);
        if state.is_stopping {
            info!("Test run stopped. Validating checksums...");
            
            let file_path = format!("{}-output.jsonl", state.cfg.topic);
            
            match Self::validate_checksums(&file_path) {
                Ok(_) => {
                    info!(
                        "{}",
                        to_string_pretty(&state.metrics).expect("expected to serialize to json")
                    );
                }
                Err(e) => {
                    error!("Failed to validate checksums: {}", e);
                }
            }
        }
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SinkAgentMsg::Start => {
                debug!("Subscribing to topic {}", state.cfg.topic);
                let msg = TcpClientMessage::Subscribe { topic: state.cfg.topic.clone(), trace_ctx: None, };
                if let Err(e) = state.tcp_client.send_message(msg) {
                    error!("Failed to send subscribe message: {}", e);
                    myself.stop(Some(format!("Subscribe failed: {}", e)));
                    return Ok(());
                }
                // Wait a bit for subscription to be processed
                tokio::time::sleep(Duration::from_millis(200)).await;
                if let Some(supervisor) = myself.try_get_supervisor() {
                    let _ = supervisor.send_message(SupervisorMessage::SinkReady);
                }
                Ok(()) // <-- explicit return
            }
            SinkAgentMsg::Receive { payload, trace_ctx } => {
                let receive_future = async {
                    let span = trace_span!("sink.process_message", topic = %state.cfg.topic);
                    if let Some(wire_ctx) = trace_ctx.as_ref() {
                        tracing::debug!("Sink processing message with trace_id: {:02x?}", wire_ctx.trace_id);
                    }
                    try_set_parent_wire(&span, trace_ctx);
                    let _g = span.enter();

                    info!("[{}] Receive ENTER", state.instance_id);
                    tracing::trace!(
                        "Receive: is_stopping={}, expected={:?}, current_received={}",
                        state.is_stopping, state.expected_count, state.metrics.received
                    );

                    // If we're already stopping, ignore this message
                    if state.is_stopping {
                        trace!("[{}] Ignoring message while stopping", state.instance_id);
                        return Ok(());
                    }

                    // --- Process the message ---
                    // Deserialize
                    let envelope = match rkyv::from_bytes::<Envelope, rancor::Error>(&payload) {
                        Ok(e) => e,
                        Err(e) => {
                            error!("Failed to deserialize envelope: {}", e);
                            return Ok(());
                        }
                    };

                    debug!("Received message {}, in sequence: {}", state.metrics.received + 1, envelope.seqno);

                    // Validate checksum
                    if !validate_checksum(envelope.data.as_bytes(), &envelope.checksum) {
                        error!("Checksum validation failed for message {}", envelope.seqno);
                    }

                    // --- Write to file using spawn_blocking with explicit error handling ---
                    let file_path = format!("{}-output.jsonl", state.cfg.topic);
                    let envelope_clone = envelope.clone();
                    let write_result: Result<Result<(), String>, tokio::task::JoinError> = tokio::task::spawn_blocking(move || {
                        use std::fs::OpenOptions;
                        use std::io::Write;

                        let mut file = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&file_path)
                            .map_err(|e| format!("Failed to open file: {}", e))?;
                        let json = serde_json::to_string(&envelope_clone)
                            .map_err(|e| format!("Failed to serialize JSON: {}", e))?;
                        writeln!(file, "{}", json)
                            .map_err(|e| format!("Failed to write to file: {}", e))?;
                        file.sync_all()
                            .map_err(|e| format!("Failed to sync file: {}", e))?;
                        Ok(())
                    }).await;

                    match write_result {
                        Ok(Ok(())) => {},
                        Ok(Err(e)) => error!("File write error for message {}: {}", envelope.seqno, e),
                        Err(e) => error!("Spawn_blocking join error for message {}: {}", envelope.seqno, e),
                    }

                    // Update metrics
                    state.metrics.received += 1;
                    trace!("[{}] Incremented received to {}", state.instance_id, state.metrics.received);

                    let now = Instant::now();
                    if let Some(last) = state.last_seen {
                        let delta = now.duration_since(last);
                        state.metrics.last_interarrival = Some(delta);
                        state.metrics.min_interarrival = Some(
                            state.metrics.min_interarrival.map_or(delta, |m| m.min(delta))
                        );
                        state.metrics.max_interarrival = Some(
                            state.metrics.max_interarrival.map_or(delta, |m| m.max(delta))
                        );
                        let n = state.metrics.received as f64;
                        let new_avg = match state.metrics.avg_interarrival {
                            Some(avg) => ((avg * (n - 1.0)) + delta.as_secs_f64()) / n,
                            None => delta.as_secs_f64(),
                        };
                        state.metrics.avg_interarrival = Some(new_avg);
                    }
                    state.last_seen.replace(now);

                    // --- After processing, check if we should shut down ---
                    if let Some(expected) = state.expected_count {
                        if state.metrics.received >= expected && !state.is_stopping {
                            info!(
                                "[{}] Reached expected message count ({}), flushing and shutting down",
                                state.instance_id, expected
                            );

                            state.is_stopping = true;

                            // Send disconnect and give it time to go out
                            if let Err(e) = state.tcp_client.send_message(TcpClientMessage::Disconnect { trace_ctx: None }) {
                                error!("Failed to send disconnect to TCP client: {}", e);
                            }
                            tokio::time::sleep(Duration::from_millis(200)).await;

                            // Stop the actor
                            myself.stop(Some("ALL_MESSAGES_RECEIVED".to_string()));
                            return Ok(());
                        }
                    }

                    Ok(())
                };

                match tokio::time::timeout(Duration::from_secs(5), receive_future).await {
                    Ok(Ok(())) => {
                        trace!("[{}] Receive handler completed successfully", state.instance_id);
                        Ok(())
                    }
                    Ok(Err(e)) => {
                        error!("[{}] Receive handler returned error: {:?}", state.instance_id, e);
                        Err(e)
                    }
                    Err(_) => {
                        error!("[{}] Receive handler timed out after 5s", state.instance_id);
                        myself.stop(Some("Receive timeout".to_string()));
                        Ok(())
                    }
                }
            }
            SinkAgentMsg::GracefulStop => {
                info!("Starting graceful shutdown sequence...");
                state.is_stopping = true;
                
                if let Err(e) = state.tcp_client.send_message(TcpClientMessage::Disconnect { trace_ctx: None }) {
                    error!("Failed to send disconnect to TCP client: {}", e);
                }

                tokio::time::sleep(Duration::from_millis(200)).await;

                if let Some(expected) = state.expected_count {
                    if state.metrics.received >= expected {
                        myself.stop(Some("GRACEFUL_SHUTDOWN".to_string()));
                    }
                } else {
                    myself.stop(Some("GRACEFUL_SHUTDOWN".to_string()));
                }
                Ok(())
            }
            SinkAgentMsg::Error { reason } => {
                // If we are already in graceful shutdown, a transport error is expected
                // (the broker may have closed the connection after our Disconnect).
                // We log it at info level and do not propagate it as a failure.
                if state.is_stopping {
                    info!(
                        "[{}] Broker connection closed during shutdown: {}",
                        state.instance_id, reason
                    );
                    return Ok(());
                }

                // Otherwise, treat this as an unexpected error and notify the supervisor.
                let supervisor = myself.try_get_supervisor().expect("Expected to find a supervisor");
                if let Err(e) = supervisor.send_message(SupervisorMessage::AgentError { reason }) {
                    error!("Failed to send AgentError to supervisor: {}", e);
                }
                Ok(())
            }
        }
    }
}
