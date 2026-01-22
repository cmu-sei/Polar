use harness_common::{
    AgentRole, ControllerCommand, SupervisorMessage,
    client::{
        ClientEvent as HarnessClientEvent, ControlClient, ControlClientArgs, ControlClientConfig,
        ControlClientMsg,
    },
};
use harness_sink::sink::SinkAgentMsg;
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};

use tracing::{debug, error, info, warn};

pub const SINK_CLIENT_SESSION: &str = "cassini.harness.sink.session";

// ============================== Sink Service ============================== //

pub struct SinkService;

pub struct SinkServiceState {
    harness_client: ActorRef<ControlClientMsg>,
    sink: Option<ActorRef<SinkAgentMsg>>,
}

#[async_trait]
impl Actor for SinkService {
    type Msg = SupervisorMessage;
    type State = SinkServiceState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!("Starting {myself:?}");
        tracing::info!("RootActor: Started {myself:?}");

        let output_port = OutputPort::default();

        output_port.subscribe(myself.clone(), |event| match event {
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
                events_output: std::sync::Arc::new(output_port),
            },
            myself.clone().into(),
        )
        .await?;

        Ok(SinkServiceState {
            harness_client,
            sink: None,
        })
    }

    async fn post_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(actor_cell) => {
                info!(
                    "Worker agent: {0:?}:{1:?} started",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorTerminated(actor_cell, _boxed_state, reason) => {
                let client_id = actor_cell
                    .get_name()
                    .expect("Expected client listener to have been named");

                info!(
                    "Client listener: {0}:{1:?} stopped. {reason:?}",
                    client_id,
                    actor_cell.get_id()
                );
                state
                    .harness_client
                    .send_message(ControlClientMsg::SendCommand(
                        ControllerCommand::TestComplete {
                            client_id: String::default(),
                            role: AgentRole::Sink,
                        },
                    ))?;
            }
            SupervisionEvent::ActorFailed(actor_cell, _) => {
                let error = format!(
                    "Worker agent: {0:?}:{1:?} failed!",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                error!("{error}");
                state
                    .harness_client
                    .send_message(ControlClientMsg::SendCommand(
                        ControllerCommand::TestError {
                            error: error.clone(),
                        },
                    ))?;
                myself.stop(Some(error))
            }
            SupervisionEvent::ProcessGroupChanged(_) => (),
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
            SupervisorMessage::ControllerConnected => {
                // say hello
                debug!("Controller connected, saying hello.");
                state
                    .harness_client
                    .send_message(ControlClientMsg::SendCommand(ControllerCommand::Hello {
                        role: AgentRole::Sink,
                    }))?;
            }
            SupervisorMessage::CommandReceived { command } => match command {
                ControllerCommand::SinkTopic { topic } => {
                    debug!("Received topic {topic}, starting sink actor");
                    let args = harness_sink::sink::SinkConfig {
                        topic: topic.clone(),
                    };

                    let (sink, _) = Actor::spawn_linked(
                        Some(format!("cassini.harness.sink.{}", topic)),
                        harness_sink::sink::SinkAgent,
                        args,
                        myself.clone().into(),
                    )
                    .await?;

                    state.sink = Some(sink);
                }
                ControllerCommand::ProducerFinished => {
                    debug!(
                        "Received message producer is finished. stopping sink client and commencing validation."
                    );
                    if let Some(sink) = state.sink.take() {
                        sink.stop(Some("PRODUCER_FINISHED".to_string()))
                    } else {
                        return Err("No sink found".into());
                    }
                }
                ControllerCommand::Shutdown => {
                    debug!(
                        "Received shutdown command. stopping sink client and commencing validation."
                    );
                    myself.stop(Some("SHUTDOWN".to_string()));
                }
                _ => {
                    warn!("Received unknown command {command:?}");
                }
            },
            SupervisorMessage::TransportError { reason } => {
                error!("Lost connection to controller: {}", reason);
                myself.stop(Some(reason));
            }
            SupervisorMessage::AgentError { reason } => {
                error!("Agent encountered an error: {reason}");
                myself.stop(Some(reason));
            }
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    polar::init_logging();

    info!("Sink agent starting up.");

    let (_, handle) = Actor::spawn(
        Some("cassini.harness.sink.supervisor".to_string()),
        SinkService,
        (),
    )
    .await
    .expect("Expected to start sink supervisor");

    handle.await.unwrap();
}
