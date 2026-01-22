// use cassini_client::*;
use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent as CassiniEvent;
use fake::Fake;
use harness_common::{
    client::{
        ClientEvent as HarnessClientEvent, ControlClient, ControlClientArgs, ControlClientConfig,
        ControlClientMsg,
    },
    compute_checksum, ControllerCommand, Envelope, MessagePattern, ProducerConfig,
    SupervisorMessage,
};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent};
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time;
use tracing::{debug, error, info, warn};
const PRODUCER_FINISHED_SUCCESSFULLY: &str = "PRODUCER_FINISHED_SUCCESSFULLY";
// const PRODUCER_ENCOUNTERED_ERROR: &str = "PRODUCER_ENCOUNTERED_ERROR";

// Simple metrics struct
#[derive(Debug, Default, Serialize, Clone)]
pub struct Metrics {
    pub sent: usize,
    pub received: usize,
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
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
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
                _ => warn!("Received unexpected command."),
            },
            SupervisorMessage::TransportError { reason } => {
                error!("Lost connection to the controller: {}", reason);
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
                tracing::error!("{dead_actor:?} failed {panic_msg}");
                myself.stop(None);
            }
            SupervisionEvent::ActorTerminated(dead_actor, _, reason) => {
                tracing::info!("{dead_actor:?} stopped {reason:?}");
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
        let events_output: std::sync::Arc<OutputPort<CassiniEvent>> =
            std::sync::Arc::new(OutputPort::default());

        // Subscribe to events (registration strings, etc.)
        events_output.subscribe(myself.clone(), |event| match event {
            CassiniEvent::Registered { .. } => Some(ProducerMessage::Start),
            CassiniEvent::MessagePublished { payload, .. } => None,
            CassiniEvent::TransportError { reason } => {
                error!("Lost connection to the message broker! {reason}");
                Some(ProducerMessage::AgentError { reason })
            }
        });

        let config = TCPClientConfig::new()?;
        // Prepare TcpClientArgs and spawn the client actor
        let tcp_args = TcpClientArgs {
            config,
            registration_id: None,
            events_output: events_output.clone(),
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
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("Running test.");
        // here, we take on different actions depending on behavior pattern
        match state.cfg.pattern {
            MessagePattern::Drip { idle_time_seconds } => {
                let interval = Duration::from_secs_f64(1.0 / idle_time_seconds as f64);
                let duration = Duration::from_secs(state.cfg.duration);
                let topic = state.cfg.topic.clone();

                let size = state.cfg.message_size.clone() as usize;
                let tcp_client = state.tcp_client.clone();

                let mut ticker = time::interval(interval);
                let end = Instant::now() + duration;

                let mut seqno = 0;

                while Instant::now() < end {
                    ticker.tick().await;
                    seqno += 1;

                    // generate a checksum for the message
                    // wrap it in an envelope
                    // send it
                    // create a payload of the desired message size using fake

                    let faked = (0..=size).fake::<String>();
                    let checksum = compute_checksum(faked.as_bytes());

                    let envelope = Envelope {
                        seqno,
                        data: faked,
                        checksum,
                    };

                    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
                        .expect("Expected to serialize payload to bytes");

                    let message = TcpClientMessage::Publish {
                        topic: topic.clone(),
                        payload: payload.into(),
                    };

                    if let Err(e) = tcp_client.send_message(message) {
                        tracing::warn!("Failed to send message {e}");
                        state.metrics.errors += 1;
                    }
                    state.metrics.sent += 1;
                }

                // when done, print metrics and exit
                info!(
                    "{}",
                    to_string_pretty(&state.metrics).expect("expected to serialize to json")
                );

                tcp_client
                    .send_message(TcpClientMessage::Disconnect)
                    .unwrap();
            }
            MessagePattern::Burst {
                idle_time_seconds,
                burst_size,
            } => {
                let interval = Duration::from_secs_f64(1.0 / idle_time_seconds as f64);
                let duration = Duration::from_secs(state.cfg.duration);
                let topic = state.cfg.topic.clone();

                let size = state.cfg.message_size.clone() as usize;
                let tcp_client = state.tcp_client.clone();

                info!("Starting test...");
                let mut ticker = time::interval(interval);
                let mut seqno = 0;
                let end = Instant::now() + duration;

                while Instant::now() < end {
                    ticker.tick().await;
                    seqno += 1;

                    let faked = (0..=size).fake::<String>();

                    let checksum = compute_checksum(faked.as_bytes());

                    let envelope = Envelope {
                        seqno,
                        data: faked,
                        checksum,
                    };

                    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
                        .expect("Expected to serialize payload to bytes");

                    // send a bulk number of messages
                    for _ in 1..burst_size {
                        let message = TcpClientMessage::Publish {
                            topic: topic.clone(),
                            payload: payload.clone().into(),
                        };

                        if let Err(e) = tcp_client.send_message(message) {
                            tracing::warn!("Failed to send message {e}");
                            state.metrics.errors += 1;
                        }

                        state.metrics.sent += 1;
                    }
                }
                // when done, print metrics and exit
                info!(
                    "{}",
                    to_string_pretty(&state.metrics).expect("expected to serialize to json")
                );

                tcp_client
                    .send_message(TcpClientMessage::Disconnect)
                    .unwrap();
            }
        }

        // die with honor.
        myself.stop(Some(PRODUCER_FINISHED_SUCCESSFULLY.to_string()));

        Ok(())
    }
}
