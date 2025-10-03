use cassini_client::*;
use fake::Fake;
use harness_common::{
    compute_checksum, Envelope, MessagePattern, ProducerConfig, ProducerMessage, SinkCommand,
    TestPlan,
};
use ractor::{
    async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef, ActorStatus, OutputPort,
    SupervisionEvent,
};
use serde::Serialize;
use std::time::{Duration, Instant};
use tokio::time;
use tracing::{debug, info};

use crate::client::{SinkClient, SinkClientArgs, SinkClientConfig, SinkClientMessage};

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
    sink_client: ActorRef<SinkClientMessage>,
    producers: u32,
    test_plan: TestPlan,
}

pub struct RootActorArguments {
    pub test_plan: TestPlan,
}

pub enum RootActorMessage {
    /// A quick signal to the supervisor containing the index of the producer that finished
    ProducerFinished,
    /// Signal that the sink has shutdown, so the supervisor can exit gracefully.
    SinkShutdown,
}

#[async_trait]
impl Actor for RootActor {
    type Msg = RootActorMessage;
    type State = RootActorState;
    type Arguments = RootActorArguments;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: RootActorArguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("RootActor: Started {myself:?}");

        let output_port = std::sync::Arc::new(OutputPort::default());

        // subscribe to output port for the client, at time of writing, the only thing the sink should say back is
        // a shutdownack, so we can turn that into a reason to stop execution
        output_port.subscribe(myself.clone(), |message| match message {
            ProducerMessage::ShutdownAck => Some(RootActorMessage::SinkShutdown),
            _ => None,
        });
        let client_args = SinkClientArgs {
            config: SinkClientConfig::new(),
            output_port,
        };

        let (sink_client, _) = Actor::spawn_linked(
            Some("cassini.harness.supervisor.client".to_string()),
            SinkClient,
            client_args,
            myself.clone().into(),
        )
        .await
        .expect("Expected to start tcp client");

        Ok(RootActorState {
            sink_client,
            producers: 0u32,
            test_plan: args.test_plan,
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // tODO: When the sink looks healthy, or acks us, start producers

        state
            .sink_client
            .send_message(SinkClientMessage::Send(
                harness_common::SinkCommand::TestPlan(state.test_plan.clone()),
            ))
            .unwrap();

        // Gotta keep track of producers somehow...
        // so we create a list of them here, and insert
        // the refs according to the index, starting at 0.

        for config in &state.test_plan.producers {
            let (p, _) = Actor::spawn_linked(
                Some(format!("cassini.harness.producer.{}", state.producers)),
                ProducerAgent,
                config.to_owned(),
                myself.clone().into(),
            )
            .await
            .expect("Expected to start producer agent");

            state.producers += 1;
        }

        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            RootActorMessage::ProducerFinished => {
                state.producers -= 1;
            }
            RootActorMessage::SinkShutdown => {
                info!("Sink has shut down. Concluding test.");
                state.sink_client.stop(None);
                _myself.stop(None);
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
                // TODO: Check if there are any children left,
                // if they're all done, we can tell the sink to start cleaning up and shutdown.
                //
                if state.producers == 0 {
                    //send a message to the sink telling it to shutdown, we'll stop when we hear about it
                    info!("Producers concluded. Telling sink to shut down");
                    state
                        .sink_client
                        .send_message(SinkClientMessage::Send(SinkCommand::Stop))
                        .expect("Expected to send command to sink.");
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
        let output_port = std::sync::Arc::new(OutputPort::default());
        let queue_output = std::sync::Arc::new(OutputPort::default());

        // subscribe self to this port
        output_port.subscribe(myself.clone(), |_| Some(ProducerMessage::Ready));

        let tcp_cfg = TCPClientConfig::new();

        let (client, _) = Actor::spawn_linked(
            None,
            TcpClientActor,
            TcpClientArgs {
                config: tcp_cfg,
                registration_id: None,
                output_port,
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
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            ProducerMessage::Ready => {
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
                            // TODO:

                            // generate a checksum for it
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
                        info!("{:?}", state.metrics);

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
                        info!("{:?}", state.metrics);

                        tcp_client
                            .send_message(TcpClientMessage::Disconnect)
                            .unwrap();
                    }
                }
            }
            _ => (),
        }

        myself.try_get_supervisor().map(|supervisor| {
            supervisor
                .send_message(RootActorMessage::ProducerFinished)
                .expect("Expected to contact supervisor");
        });

        Ok(())
    }
}
