use cassini_client::*;

use harness_common::{ArchivedSinkCommand, ProducerMessage, SinkCommand};
use ractor::{
    Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait,
    concurrency::{Duration, Instant, JoinHandle},
};
use rkyv::{
    deserialize,
    rancor::{self, Error, Source},
};
use rustls::{
    RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    server::WebPkiClientVerifier,
};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf, split},
    net::{TcpListener, TcpStream},
    sync::Mutex,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tracing::{debug, error, info, warn};

// ============================== Sink Actor Definition ============================== //
//

// Metrics tracked by the sink
#[derive(Debug, Default, Clone)]
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

pub struct SinkConfig {
    topic: String,
    // TODO: What else would we use to configure the sink?
}

pub struct SinkState {
    cfg: SinkConfig, //TODO: create sink specific configuration
    metrics: SinkMetrics,
    last_seen: Option<Instant>,
    tcp_client: ActorRef<TcpClientMessage>,
}

pub enum SinkAgentMsg {
    Start,
    Receive(Vec<u8>),
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

        // define an output port for the actor to subscribe to
        let output_port = std::sync::Arc::new(OutputPort::default());
        let queue_output = std::sync::Arc::new(OutputPort::default());

        // subscribe self to this port
        output_port.subscribe(myself.clone(), |_| Some(SinkAgentMsg::Start));

        // subscribe to the messaging queue, when acting as a sink, messages will be deserialized and analyzed
        queue_output.subscribe(myself.clone(), |message: Vec<u8>| {
            Some(SinkAgentMsg::Receive(message))
        });

        let tcp_cfg = TCPClientConfig::new();

        let (client, _) = Actor::spawn_linked(
            Some("cassini.harness.tcp.sink".to_string()),
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

        Ok(SinkState {
            cfg: args,
            metrics: SinkMetrics::default(),
            tcp_client: client,
            last_seen: None,
        })
    }

    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} stopped.");
        debug!("{:?}", state.metrics);

        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SinkAgentMsg::Start => {
                let msg = TcpClientMessage::Subscribe(state.cfg.topic.clone());
                state.tcp_client.send_message(msg).unwrap();
            }
            SinkAgentMsg::Receive(_payload) => {
                state.metrics.received += 1;

                debug!("Received message {}", state.metrics.received);

                let now = Instant::now();

                if let Some(last) = state.last_seen {
                    let delta = now.duration_since(last);

                    state.metrics.last_interarrival = Some(delta);

                    state.metrics.min_interarrival = Some(
                        state
                            .metrics
                            .min_interarrival
                            .map_or(delta, |m| m.min(delta)),
                    );

                    state.metrics.max_interarrival = Some(
                        state
                            .metrics
                            .max_interarrival
                            .map_or(delta, |m| m.max(delta)),
                    );

                    // Update rolling average
                    let n = state.metrics.received as f64;
                    let new_avg = match state.metrics.avg_interarrival {
                        Some(avg) => ((avg * (n - 1.0)) + delta.as_secs_f64()) / n,
                        None => delta.as_secs_f64(),
                    };
                    state.metrics.avg_interarrival = Some(new_avg);
                }

                state.last_seen.replace(now);
            }
        }
        Ok(())
    }
}
