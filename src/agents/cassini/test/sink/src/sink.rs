use cassini_client::*;

use harness_common::{
    ArchivedSinkCommand, Envelope, ProducerMessage, SinkCommand, compute_checksum,
    validate_checksum,
};
use ractor::{
    Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait,
    concurrency::{Duration, Instant, JoinHandle},
};
use rkyv::{
    deserialize,
    rancor::{self, Error, Source},
};

use serde_json::Value;
use std::path::Path;
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

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
    pub topic: String,
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

        // Serialize to compact JSON string
        let json_str = serde_json::to_string(value).expect("serialization should not fail");

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
            let message: Envelope = serde_json::from_str(&line)
                .unwrap_or_else(|_| panic!("Invalid JSON at line {}", i + 1));

            if !validate_checksum(message.data.as_bytes(), &message.checksum) {
                error!("Failed to validate checksum for message: {}", message.seqno);
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
        info!("Test run stopped. Validating checksums...");
        SinkAgent::validate_checksums(format!("{}-output.json", state.cfg.topic).as_str())
            .expect("Expected to validate checksums");
        info!("{:?}", state.metrics);
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
            SinkAgentMsg::Receive(payload) => {
                state.metrics.received += 1;

                // TODO: Do we want to panic? Or just write the error to disk and move on?
                // The latter might be ideal.
                let envelope = rkyv::from_bytes::<Envelope, rancor::Error>(&payload)
                    .expect("Expected to deserialize successfully");

                debug!(
                    "Received message {}, in sequence: {}",
                    state.metrics.received, envelope.seqno
                );
                let file_path = format!("{}-output.json", state.cfg.topic);

                // write
                SinkAgent::append_json_to_file(file_path, &envelope)
                    .await
                    .expect("Expected to write to file.");

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
