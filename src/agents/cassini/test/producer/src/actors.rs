use cassini_client::*;

use fake::Fake;
use ractor::{
    async_trait, concurrency::Interval, Actor, ActorProcessingErr, ActorRef, OutputPort,
    SupervisionEvent,
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::time;
use tracing::{debug, info};

use crate::client::{SinkClient, SinkClientArgs, SinkClientConfig, SinkClientMessage};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestPlan {
    producers: Vec<ProducerConfig>,
}
/// Messaging pattern that mimics user behavior over the network.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MessagePattern {
    Burst { idle_time: usize, burst_size: u32 },
    Drip { idle_time: usize },
}
// Config for the supervisor
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProducerConfig {
    pub topic: String,
    #[serde(alias = "msgSize")]
    pub msg_size: usize,
    pub duration: u64,
    pub pattern: MessagePattern,
}

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
    producers: Vec<ActorRef<ProducerMessage>>,
    test_plan: TestPlan,
}

pub struct RootActorArguments {
    pub test_plan: TestPlan,
}

#[async_trait]
impl Actor for RootActor {
    type Msg = ();
    type State = RootActorState;
    type Arguments = RootActorArguments;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: RootActorArguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("RootActor: Started {myself:?}");

        let output_port = std::sync::Arc::new(OutputPort::default());

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

        // contact the sink

        sink_client
            .send_message(SinkClientMessage::Send(
                harness_common::SinkCommand::HealthCheck,
            ))
            .unwrap();

        Ok(RootActorState {
            sink_client,
            producers: Vec::new(),
            test_plan: args.test_plan,
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // tODO: When the sink looks healthy, or acks us, start producers

        // let mut count = 0;
        // for config in &state.test_plan.producers {
        //     count += 1;
        //     let (p, _) = Actor::spawn_linked(
        //         Some(format!("cassini.harness.producer.{count}")),
        //         ProducerAgent,
        //         config.to_owned(),
        //         myself.clone().into(),
        //     )
        //     .await
        //     .expect("Expected to start producer agent");

        //     state.producers.push(p);
        // }
        // let _ = handle.await;
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

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisionEvent::ActorFailed(dead_actor, panic_msg) => {
                tracing::info!("{dead_actor:?} failed {panic_msg}");
                myself.stop(None);
            }
            SupervisionEvent::ActorTerminated(dead_actor, reason, ..) => {
                tracing::info!("{dead_actor:?} stopped {reason:?}");
                // stop sink
                myself.stop_children_and_wait(None, None).await;
                myself.stop(None);
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

// Messages the supervisor handles
#[derive(Debug)]
pub enum ProducerMessage {
    /// Signal to start sending messages, received after successful registration with the broker
    Start,
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
        output_port.subscribe(myself.clone(), |_| Some(ProducerMessage::Start));

        let tcp_cfg = TCPClientConfig::new();

        let (client, _) = Actor::spawn_linked(
            Some("cassini.harness.tcp.producer".to_string()),
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
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            ProducerMessage::Start => {
                // here, we take on different actions depending on behavior pattern
                match state.cfg.pattern {
                    MessagePattern::Drip { idle_time } => {
                        let interval = Duration::from_secs_f64(1.0 / idle_time as f64);
                        let duration = Duration::from_secs(state.cfg.duration);
                        let topic = state.cfg.topic.clone();

                        let size = state.cfg.msg_size.clone();
                        let tcp_client = state.tcp_client.clone();

                        info!("Starting test...");
                        tokio::spawn(async move {
                            let mut ticker = time::interval(interval);
                            let end = Instant::now() + duration;
                            let mut metrics = Metrics::default();

                            while Instant::now() < end {
                                ticker.tick().await;

                                // TODO:
                                // Instead of faking the data here, serialize a given value (some arbitrary data structure)
                                // generate a checksum for it
                                // wrap it in an envelope
                                // send it
                                // create a payload of the desired message size using fake
                                let faked = (0..=size).fake::<String>();

                                let payload = faked.as_bytes().to_owned();

                                let message = TcpClientMessage::Publish {
                                    topic: topic.clone(),
                                    payload,
                                };

                                if let Err(e) = tcp_client.send_message(message) {
                                    tracing::warn!("Failed to send message {e}");
                                    metrics.errors += 1;
                                }
                                metrics.sent += 1;
                            }
                            // when done, print metrics and exit
                            info!("{:?}", metrics);

                            tcp_client
                                .send_message(TcpClientMessage::Disconnect)
                                .unwrap();
                        });
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
                        tokio::spawn(async move {
                            let mut ticker = time::interval(interval);
                            let end = Instant::now() + duration;
                            let mut metrics = Metrics::default();

                            while Instant::now() < end {
                                ticker.tick().await;

                                let faked = (0..=size).fake::<String>();

                                let payload = faked.as_bytes().to_owned();

                                // send a bulk number of messages
                                for _ in 1..burst_size {
                                    let message = TcpClientMessage::Publish {
                                        topic: topic.clone(),
                                        payload: payload.clone(),
                                    };

                                    if let Err(e) = tcp_client.send_message(message) {
                                        tracing::warn!("Failed to send message {e}");
                                        metrics.errors += 1;
                                    }

                                    metrics.sent += 1;
                                }
                            }
                            // when done, print metrics and exit
                            info!("{:?}", metrics);

                            tcp_client
                                .send_message(TcpClientMessage::Disconnect)
                                .unwrap();
                        });
                    }
                }
            }
        }

        Ok(())
    }
}
