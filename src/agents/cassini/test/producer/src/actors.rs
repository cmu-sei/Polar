// use cassini_client::*;
use cassini_client::{
    ClientEventForwarder, ClientEventForwarderArgs, TCPClientConfig, TcpClientActor,
    TcpClientArgs, TcpClientMessage,
};
use cassini_types::ClientEvent as CassiniEvent;
use fake::Fake;
use harness_common::{
    client::{
        ClientEvent as HarnessClientEvent, ControlClient, ControlClientArgs, ControlClientConfig,
        ControlClientMsg,
    },
    compute_checksum, ControllerCommand, Envelope, MessagePattern, ProducerConfig,
    SupervisorMessage, WireTraceCtx,
};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent};
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::{self, sleep};
use tracing::{debug, error, info, warn, info_span};
const PRODUCER_FINISHED_SUCCESSFULLY: &str = "PRODUCER_FINISHED_SUCCESSFULLY";
// const PRODUCER_ENCOUNTERED_ERROR: &str = "PRODUCER_ENCOUNTERED_ERROR";

// Simple metrics struct
#[derive(Debug, Default, Serialize, Clone)]
pub struct Metrics {
    pub sent: usize,
    pub errors: usize,
    pub start_ms: u128,
    pub elapsed_ms: u128,
}

// ============================== Root Actor Definition ============================== //
/// This just exists to await the conclusion of the test, the real "harness" of the framework.
pub struct RootActor;

pub struct RootActorState {
    harness_client: ActorRef<ControlClientMsg>,
    producer: Option<ActorRef<ProducerMessage>>,
    work_completed: bool,  // true when the producer agent finished successfully
}

#[async_trait]
impl Actor for RootActor {
    type Msg = SupervisorMessage;
    type State = RootActorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("RootActor: Started {myself:?}");

        let events_output = Arc::new(OutputPort::default());

        // subscribe self to this port
        events_output.subscribe(myself.clone(), |event| match event {
            HarnessClientEvent::CommandReceived { command } => {
                Some(SupervisorMessage::CommandReceived { command })
            }
            HarnessClientEvent::Connected => Some(SupervisorMessage::ControllerConnected),
            HarnessClientEvent::TransportError { reason } => {
                tracing::error!("Disconnected from harness: {reason}");
                Some(SupervisorMessage::TransportError { reason })
            }
        });

        let (harness_client, _) = Actor::spawn_linked(
            Some("cassini.harness.producer.client".to_string()),
            ControlClient,
            ControlClientArgs {
                config: ControlClientConfig::new()?,
                events_output,
            },
            myself.clone().into(),
        )
        .await?;

        Ok(RootActorState {
            harness_client,
            producer: None,
            work_completed: false,
        })
    }

    async fn post_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::ControllerConnected => {
                debug!("Connected to controller successfully");
                // say hello
                state.harness_client.cast(ControlClientMsg::SendCommand(
                    ControllerCommand::Hello {
                        role: harness_common::AgentRole::Producer,
                    },
                ))?;
            }
            SupervisorMessage::SinkReady => {
                debug!("Received SinkReady (ignoring)");
            }
            SupervisorMessage::CommandReceived { command } => match command {
                ControllerCommand::ProducerConfig { producer } => {
                    debug!(
                        "Received producer configuration: {}",
                        serde_json::to_string_pretty(&producer).unwrap()
                    );

                    let (producer, _) = Actor::spawn_linked(
                        Some(format!(
                            "cassini.harness.producer.{}",
                            producer.topic.clone()
                        )),
                        ProducerAgent,
                        producer,
                        myself.clone().into(),
                    )
                    .await?;

                    state.producer = Some(producer);
                }
                ControllerCommand::Shutdown => {
                    debug!(
                        "Received shutdown command. stopping sink client and commencing validation."
                    );
                    myself.stop(Some("SHUTDOWN".to_string()));
                }
                _ => warn!("Received unexpected command."),
            },
            SupervisorMessage::TransportError { reason } => {
                if state.work_completed {
                    info!("Controller disconnected after work completed (shutdown): {}", reason);
                } else {
                    error!("Lost connection to the controller: {}", reason);
                }
                myself.stop(Some(reason))
            }
            SupervisorMessage::AgentError { reason } => {
                error!("Producer encountered an error! {} Stopping.", reason);
                myself.stop(Some(reason))
            }
        }

        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisionEvent::ActorFailed(dead_actor, panic_msg) => {
                let error = format!(
                    "Worker agent: {0:?}:{1:?} failed! {panic_msg}",
                    dead_actor.get_name(),
                    dead_actor.get_id()
                );
                error!("{error}");
                state
                    .harness_client
                    .send_message(ControlClientMsg::SendCommand(
                        ControllerCommand::TestError {
                            error: error.clone(),
                        },
                    ))?;
                myself.stop(Some(error));
            }
            SupervisionEvent::ActorTerminated(dead_actor, _, reason) => {
                let actor_name = dead_actor.get_name().unwrap_or_default();
                let actor_id = dead_actor.get_id();
                tracing::info!("Worker agent: {0}:{1:?} stopped {reason:?}", actor_name, actor_id);

                // If this is the producer agent, mark work as completed and stop supervisor
                if let Some(producer_ref) = &state.producer {
                    if producer_ref.get_id() == actor_id {
                        state.work_completed = true;
                        state.producer = None;

                        // Notify controller that the producer has finished its test
                        state.harness_client.send_message(ControlClientMsg::SendCommand(
                                ControllerCommand::TestComplete {
                                    client_id: String::default(),
                                    role: harness_common::AgentRole::Producer,
                                },
                            ))?;

                        info!("Producer agent finished, delaying stop to allow message delivery");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

                        info!("Stopping supervisor now");
                        myself.stop(Some("Producer finished".to_string()));
                        return Ok(());
                    }
                }

                // If no children remain, stop supervisor (failsafe)
                if myself.get_children().is_empty() {
                    info!("No children left, stopping supervisor");
                    myself.stop(Some("All children terminated".to_string()));
                }
            }
            other => {
                tracing::info!("RootActor: received supervisor event '{other}'");
            }
        }
        Ok(())
    }
}

pub struct ProducerAgent;

pub struct ProducerState {
    cfg: ProducerConfig,
    metrics: Metrics,
    tcp_client: ActorRef<TcpClientMessage>,
}

pub enum ProducerMessage {
    Start, // trigger tests
    CassiniEvent(CassiniEvent),
    AgentError { reason: String },
}

#[async_trait]
impl Actor for ProducerAgent {
    type Msg = ProducerMessage;
    type State = ProducerState;
    type Arguments = ProducerConfig;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        // Create the output ports the client expects
        let (forwarder, _) = Actor::spawn_linked(
            Some(format!("producer.{}.event-forwarder", args.topic)),
            ClientEventForwarder::new(),
            ClientEventForwarderArgs {
                target: myself.clone(),
                mapper: Box::new(|event| match event {
                    CassiniEvent::Registered { .. } => Some(ProducerMessage::Start),
                    CassiniEvent::MessagePublished { .. } => None,
                    CassiniEvent::ControlResponse { .. } => None,
                    CassiniEvent::TransportError { reason } => {
                        Some(ProducerMessage::AgentError { reason })
                    }
                }),
            },
            myself.clone().into(),
        )
        .await?;


        let config = TCPClientConfig::new()?;
        // Prepare TcpClientArgs and spawn the client actor
        let tcp_args = TcpClientArgs {
            config,
            registration_id: None,
            events_output: None,
            event_handler: Some(forwarder.into()),
        };

        let (tcp_client, _) = TcpClientActor::spawn_linked(
            Some(format!("producer.{}.cassini.tcp", args.topic)),
            TcpClientActor,
            tcp_args,
            myself.clone().into(),
        )
        .await?;

        let state = ProducerState {
            cfg: args,
            metrics: Metrics::default(),
            tcp_client,
        };

        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} started.");
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        _msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("Running test.");

        // Create a root span for the entire test
        let test_span = info_span!(
            "cassini.producer.test_run",
            topic = %state.cfg.topic,
            duration_seconds = state.cfg.duration
        );
        let _g = test_span.enter();

        match state.cfg.pattern {
            MessagePattern::Drip { idle_time_seconds } => {
                let interval = Duration::from_secs_f64(1.0 / idle_time_seconds as f64);
                let duration = Duration::from_secs(state.cfg.duration);
                let topic = state.cfg.topic.clone();

                let size = state.cfg.message_size as usize;
                let tcp_client = state.tcp_client.clone();

                let mut ticker = time::interval(interval);
                let end = Instant::now() + duration;
                state.metrics.start_ms = crate::get_timestamp_in_milliseconds()?;

                let mut seqno = 0;

                while Instant::now() < end {
                    ticker.tick().await;
                    seqno += 1;

                    // Create a span for each message
                    let message_span = info_span!(
                        "cassini.producer.send_message",
                        seqno = seqno,
                        topic = %topic
                    );
                    let _g = message_span.enter();

                    // Capture the trace context from the current span
                    let trace_ctx_opt = WireTraceCtx::from_current_span();
                    if let Some(ctx) = trace_ctx_opt {
                        tracing::debug!("Producer sending message with trace_id: {:02x?}", ctx.trace_id);
                    }
                    // Extract the inner value for the envelope (non‑optional)
                    let trace_ctx_inner = trace_ctx_opt.expect("active span should have a valid context");

                    let faked = (0..=size).fake::<String>();
                    let checksum = compute_checksum(faked.as_bytes());

                    let envelope = Envelope {
                        seqno,
                        data: faked,
                        checksum,
                        trace_ctx: trace_ctx_inner,   // envelope expects WireTraceCtx (non‑optional)
                    };

                    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
                        .expect("Expected to serialize payload to bytes");

                    let message = TcpClientMessage::Publish {
                        topic: topic.clone(),
                        payload: payload.into(),
                        trace_ctx: Some(trace_ctx_inner),   // client message expects Option
                    };

                    if let Err(e) = tcp_client.send_message(message) {
                        tracing::warn!("Failed to send message {e}");
                        state.metrics.errors += 1;
                    }
                    state.metrics.sent += 1;
                }

                state.metrics.elapsed_ms = crate::get_timestamp_in_milliseconds()? - state.metrics.start_ms;

                // Wait for any pending acknowledgments before disconnecting
                debug!("Waiting for pending acknowledgments...");
                sleep(Duration::from_millis(200)).await;

                info!(
                    "{}",
                    to_string_pretty(&state.metrics).expect("expected to serialize to json")
                );

                tcp_client
                    .send_message(TcpClientMessage::Disconnect { trace_ctx: None })
                    .unwrap();
            }

            MessagePattern::Burst {
                idle_time_seconds,
                burst_size,
            } => {
                let interval = Duration::from_secs_f64(1.0 / idle_time_seconds as f64);
                let duration = Duration::from_secs(state.cfg.duration);
                let topic = state.cfg.topic.clone();

                let size = state.cfg.message_size as usize;
                let tcp_client = state.tcp_client.clone();

                state.metrics.start_ms = crate::get_timestamp_in_milliseconds()?;

                let mut ticker = time::interval(interval);
                let mut seqno = 0;
                let end = Instant::now() + duration;

                while Instant::now() < end {
                    ticker.tick().await;
                    seqno += 1;

                    // Create a span for each message
                    let message_span = info_span!(
                        "cassini.producer.send_message",
                        seqno = seqno,
                        topic = %topic
                    );
                    let _g = message_span.enter();

                    // Capture the trace context from the current span
                    let trace_ctx_opt = WireTraceCtx::from_current_span();
                    if let Some(ctx) = trace_ctx_opt {
                        tracing::debug!("Producer sending message with trace_id: {:02x?}", ctx.trace_id);
                    }
                    let trace_ctx_inner = trace_ctx_opt.expect("active span should have a valid context");

                    let faked = (0..=size).fake::<String>();
                    let checksum = compute_checksum(faked.as_bytes());

                    let envelope = Envelope {
                        seqno,
                        data: faked,
                        checksum,
                        trace_ctx: trace_ctx_inner,   // envelope expects non‑optional
                    };

                    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
                        .expect("Expected to serialize payload to bytes");

                    // Send a burst of messages (all using the same trace context)
                    for _ in 1..burst_size {
                        let message = TcpClientMessage::Publish {
                            topic: topic.clone(),
                            payload: payload.clone().into(),
                            trace_ctx: Some(trace_ctx_inner),   // client message expects Option
                        };

                        if let Err(e) = tcp_client.send_message(message) {
                            tracing::warn!("Failed to send message {e}");
                            state.metrics.errors += 1;
                        }
                        state.metrics.sent += 1;
                    }
                }

                state.metrics.elapsed_ms =
                    crate::get_timestamp_in_milliseconds()? - state.metrics.start_ms;

                debug!("Waiting for pending acknowledgments...");
                sleep(Duration::from_millis(200)).await;

                info!(
                    "{}",
                    to_string_pretty(&state.metrics).expect("expected to serialize to json")
                );

                tcp_client
                    .send_message(TcpClientMessage::Disconnect { trace_ctx: None })
                    .unwrap();
            }
        }

        // die with honor.
        myself.stop(Some(PRODUCER_FINISHED_SUCCESSFULLY.to_string()));

        Ok(())
    }
}
