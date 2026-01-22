// use cassini_client::*;
use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use fake::Fake;
use harness_common::{
    Envelope, MessagePattern, ProducerConfig, client::{ControlClientArgs, ControlClientConfig, HarnessClient, HarnessClientArgs, HarnessClientConfig, HarnessClientMessage}, compute_checksum
};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent};
use serde::Serialize;
use serde_json::to_string_pretty;
use std::time::{Duration, Instant};
use tokio::time;
use tokio_util::sync::CancellationToken;
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
    harness_client: ActorRef<HarnessClientMessage>,
    producers: u32,
    registration: harness_common::,
    timeout_token: CancellationToken,
}

pub struct RootActorArguments {
    pub tcp_client_config: HarnessClientConfig,
}

#[async_trait]
impl Actor for RootActor {
    type Msg = ControllerCommand;
    type State = RootActorState;
    type Arguments = RootActorArguments;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: RootActorArguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("RootActor: Started {myself:?}");

        let events_output = OutputPort::default();

        events_output.subscribe(myself.clone(), |event| Some());
        let args = ControlClientArgs {
            config: ControlClientConfig::new(),

        }
        let (harness_client, _) = Actor::spawn_linked(
            Some("cassini.harness.producer.client".to_string()),
            HarnessClient,
            HarnessClientArgs {
                config: args.tcp_client_config,
            },
            myself.clone().into(),
        )
        .await
        .expect("Expected to start tcp client");

        Ok(RootActorState {
            harness_client,
            producers: 0u32,
            registration: ConnectionState::NotContacted,
            timeout_token: CancellationToken::new(),
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // TODO: Make configurable?
        let timeout = 60;
        let token = state.timeout_token.clone();
        let _ = tokio::spawn(async move {
            info!("Waiting {timeout} secs for contact from controller.");
            tokio::select! {
                // Use cloned token to listen to cancellation requests
                _ = token.cancelled() => {
                    info!("Cancelling timeout.")
                }
                // wait
                _ = tokio::time::sleep(std::time::Duration::from_secs(timeout)) => {
                    error!("Failed to receive contact from controller. Shutting down.");
                    myself.stop(Some("TEST_TIMED_OUT".to_string()));

                }
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
            HarnessControllerMessage::ClientRegistered(id) => {
                info!("Received client id \"{id}\"");
                state.registration = harness_common::ConnectionState::Registered { client_id: id };
            }
            HarnessControllerMessage::TestPlan { plan } => {
                state.timeout_token.cancel();

                debug!(
                    "{myself:?} Received test plan:\n{plan}",
                    plan = serde_json::to_string_pretty(&plan).unwrap()
                );
                // Gotta keep track of producers somehow...
                // so we create a list of them here, and insert
                // the refs according to the index, starting at 0.

                for config in &plan.producers {
                    let (_, _) = Actor::spawn_linked(
                        Some(format!("cassini.harness.producer.{}", state.producers)),
                        ProducerAgent,
                        config.to_owned(),
                        myself.clone().into(),
                    )
                    .await
                    .expect("Expected to start producer agent");

                    state.producers += 1;
                }
            }
            HarnessControllerMessage::Shutdown => {
                info!("Received shutdown command. Stopping...");

                state
                    .harness_client
                    .cast(HarnessClientMessage::Send(
                        HarnessControllerMessage::ShutdownAck,
                    ))
                    .ok();

                myself.stop_children(None);
                myself.stop(None);
            }
            _ => (),
        }

        // match (state.registration, message) {
        //     (ConnectionState::NotContacted, _) => error!("Can't do anything without a client ID"),

        // }
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
                // TODO: if we ran into an error, handle it.
                // if they're all done, we can tell the sink to start cleaning up and shutdown.
                //
                if state.producers == 0 {
                    //send a message to the sink telling it to shutdown, we'll stop when we hear about it
                    info!("Producers concluded. Shutting down");

                    myself.stop_children(None);
                    myself.stop(None);

                    // state
                    //     .harness_client
                    //     .send_message(SinkClientMessage::Send(HarnessContro))
                    //     .expect("Expected to send command to sink.");
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

        // define an output port for the actor to subscribe to
        let events_output = std::sync::Arc::new(OutputPort::default());
        let queue_output = std::sync::Arc::new(OutputPort::default());

        // subscribe self to this port
        // message doesn't matter, we just need to wait until the cassini client
        // registers to trigger the tests.
        events_output.subscribe(myself.clone(), |_| Some(ProducerMessage::Start));

        let tcp_cfg = TCPClientConfig::new()?;

        let (client, _) = Actor::spawn_linked(
            Some(format!("{}.tcp.client", myself.get_name().unwrap())),
            TcpClientActor,
            TcpClientArgs {
                config: tcp_cfg,
                registration_id: None,
                events_output,
                queue_output,
            },
            myself.clone().into(),
        )
        .await
        .expect("expected client to start");

        let state = ProducerState {
            cfg: args,
            metrics: Metrics::default(),
            tcp_client: client,
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
            MessagePattern::Drip { idle_time } => {
                let interval = Duration::from_secs_f64(1.0 / idle_time as f64);
                let duration = Duration::from_secs(state.cfg.duration);
                let topic = state.cfg.topic.clone();

                let size = state.cfg.msg_size.clone();
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
                idle_time,
                burst_size,
            } => {
                let interval = Duration::from_secs_f64(1.0 / idle_time as f64);
                let duration = Duration::from_secs(state.cfg.duration);
                let topic = state.cfg.topic.clone();

                let size = state.cfg.msg_size.clone();
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
