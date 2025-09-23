use cassini_client::*;
use fake::Fake;
use ractor::{
    async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort, SupervisionEvent,
};
use serde::Serialize;
use std::time::{Duration, Instant};
use tokio::time;
use tracing::{debug, info};

// Role for the supervisor
#[derive(Clone, Debug, Serialize)]
pub enum Role {
    Producer,
    Consumer,
}

// Config for the supervisor
#[derive(Clone, Debug, Serialize)]
pub struct AgentConfig {
    pub role: Role,
    pub topic: String,
    pub msg_size: usize,
    pub rate: u32,
    pub duration: u64,
}

// Messages the supervisor handles
#[derive(Debug)]
pub enum AgentMsg {
    /// Signal to start sending messages, received after successful registration with the broker
    Start,
    Receive(Vec<u8>),
    Stop,
    GetMetrics(RpcReplyPort<Metrics>),
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

pub struct RootActorState;

#[async_trait]
impl Actor for RootActor {
    type Msg = ();
    type State = RootActorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("RootActor: Started {myself:?}");
        Ok(RootActorState)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
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
                tracing::error!("{dead_actor:?} failed {panic_msg}");
                myself.stop(None);
            }
            SupervisionEvent::ActorTerminated(dead_actor, reason, ..) => {
                tracing::error!("{dead_actor:?} stopped {reason:?}");
                myself.stop(None);
            }
            other => {
                tracing::info!("RootActor: received supervisor event '{other}'");
            }
        }
        Ok(())
    }
}

// Supervisor state
pub struct AgentState {
    cfg: AgentConfig,
    metrics: Metrics,
    tcp_client: ActorRef<TcpClientMessage>,
}

pub struct HarnessAgent;

#[async_trait]
impl Actor for HarnessAgent {
    type Msg = AgentMsg;
    type State = AgentState;
    type Arguments = AgentConfig;

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
        output_port.subscribe(myself.clone(), |_| Some(AgentMsg::Start));

        // subscribe to the messaging queue, when acting as a sink, messages will be deserialized and analyzed
        queue_output.subscribe(myself.clone(), |message: Vec<u8>| {
            Some(AgentMsg::Receive(message))
        });

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

        let state = AgentState {
            cfg: args,
            metrics: Metrics::default(),
            tcp_client: client,
        };

        Ok(state)
    }

    async fn post_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
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
            AgentMsg::Start => {
                match state.cfg.role {
                    Role::Producer => {
                        let rate = state.cfg.rate;
                        let interval = Duration::from_secs_f64(1.0 / rate as f64);
                        let duration = Duration::from_secs(state.cfg.duration);
                        let topic = state.cfg.topic.clone();

                        let myself_clone = myself.clone();
                        let size = state.cfg.msg_size.clone();
                        let tcp_client = state.tcp_client.clone();

                        info!("Starting test...");
                        tokio::spawn(async move {
                            let mut ticker = time::interval(interval);
                            let end = Instant::now() + duration;

                            while Instant::now() < end {
                                ticker.tick().await;

                                // create a payload of the desired message size using fake

                                let faked = (0..=size).fake::<String>();

                                let payload = faked.as_bytes().to_owned();

                                let message = TcpClientMessage::Publish {
                                    topic: topic.clone(),
                                    payload,
                                };

                                if let Err(e) = tcp_client.send_message(message) {
                                    tracing::warn!("Failed to send message {e}");
                                    myself.stop(Some(e.to_string()));
                                }
                            }
                            // When done, send Stop to self
                            myself_clone.send_message(AgentMsg::Stop).unwrap();
                        });
                    }
                    Role::Consumer => {
                        debug!("Subscribing to topic: {}", state.cfg.topic);
                        let msg = TcpClientMessage::Subscribe(state.cfg.topic.clone());
                        state.tcp_client.send_message(msg).unwrap();
                    }
                }
            }
            AgentMsg::Receive(_) => {
                //validate the payload
                state.metrics.received += 1;

                debug!("Received message {}", state.metrics.received);
            }
            AgentMsg::Stop => {
                info!("Supervisor {:?} stopping", state.cfg.role);

                info!("Metrics {:?}", state.metrics);
                myself.stop(None);
            }

            AgentMsg::GetMetrics(reply) => {
                let mut m = state.metrics.clone();
                m.elapsed_ms = Instant::now().elapsed().as_millis() - m.start_ms;
                reply.send(m).unwrap();
            }
        }

        Ok(())
    }
}
