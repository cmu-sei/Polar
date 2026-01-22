use harness_common::{
    AgentRole, ControllerCommand, SupervisorMessage,
    client::{
        ClientEvent as HarnessClientEvent, ControlClient, ControlClientArgs, ControlClientConfig,
        ControlClientMsg,
    },
};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};
use tokio_util::sync::CancellationToken;

use tracing::{debug, error, info, warn};

pub const SINK_CLIENT_SESSION: &str = "cassini.harness.sink.session";

// ============================== Sink Service ============================== //

pub struct SinkService;

pub struct SinkServiceState {
    harness_client: ActorRef<ControlClientMsg>,
    timeout_token: CancellationToken,
}

#[async_trait]
impl Actor for SinkService {
    type Msg = SupervisorMessage;
    type State = SinkServiceState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
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
        .await
        .expect("Expected to start tcp client");

        Ok(SinkServiceState {
            harness_client,
            timeout_token: CancellationToken::new(),
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _: &mut Self::State,
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
            }
            SupervisionEvent::ActorFailed(actor_cell, _) => {
                error!(
                    "Worker agent: {0:?}:{1:?} failed!",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                // TODO: When this happens, send a message to the producer client
                // should we
                // Kill the whole test
                // kill the corresponding producer? if so how?
                // do nothing?
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

                    let _ = Actor::spawn_linked(
                        Some(format!("cassini.harness.sink.{}", topic)),
                        harness_sink::sink::SinkAgent,
                        args,
                        myself.clone().into(),
                    )
                    .await?;
                }
                _ => {
                    warn!("Received unknown command");
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
    .expect("Expected to start sink server");

    handle.await.unwrap();
}
