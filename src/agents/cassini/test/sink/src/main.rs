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
use cassini_client::{init_tracing, shutdown_tracing};
use std::time::Duration;
use std::panic;

// ============================== Sink Service ============================== //

pub struct SinkService;

pub struct SinkServiceState {
    harness_client: ActorRef<ControlClientMsg>,
    sink: Option<ActorRef<SinkAgentMsg>>,
    work_completed: bool,
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

        // Build config; if it fails, log and return error (supervisor will stop)
        let config = match ControlClientConfig::new() {
            Ok(cfg) => cfg,
            Err(e) => {
                error!("Failed to create control client config: {}", e);
                return Err(e);
            }
        };

        // Spawn control client; handle error
        let (harness_client, _) = match Actor::spawn_linked(
            Some("cassini.harness.producer.client".to_string()),
            ControlClient,
            ControlClientArgs {
                config,
                events_output: std::sync::Arc::new(output_port),
            },
            myself.clone().into(),
        )
        .await
        {
            Ok(result) => result, // result is (ActorRef, JoinHandle)
            Err(e) => {
                error!("Failed to spawn control client: {}", e);
                return Err(ActorProcessingErr::from(e));
            }
        };

        Ok(SinkServiceState {
            harness_client,
            sink: None,
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

    async fn post_stop(&self, _myself: ActorRef<Self::Msg>, _state: &mut Self::State) -> Result<(), ActorProcessingErr> {
        info!("Sink supervisor stopped");
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
                    "Worker agent: {:?}:{:?} started",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorTerminated(actor_cell, _boxed_state, reason) => {
                let actor_name = actor_cell.get_name().unwrap_or_default();
                let actor_id = actor_cell.get_id();
                info!(
                    "Client listener: {0}:{1:?} stopped. {reason:?}",
                    actor_name, actor_id
                );

                if let Some(sink_ref) = &state.sink {
                    info!("Current sink ref ID: {:?}", sink_ref.get_id());
                } else {
                    info!("Sink ref is None at termination time");
                }

                if let Some(sink_ref) = &state.sink {
                    if sink_ref.get_id() == actor_id {
                        state.work_completed = true;
                        state.sink = None;

                        // Check if termination was graceful
                        let is_graceful = match reason.as_deref() {
                            Some("GRACEFUL_SHUTDOWN") | Some("ALL_MESSAGES_RECEIVED") => true,
                            _ => false,
                        };

                        if is_graceful {
                            // Normal completion – notify controller
                            info!("Sending TestComplete to controller for sink");
                            if let Err(e) = state.harness_client.send_message(ControlClientMsg::SendCommand(
                                ControllerCommand::TestComplete {
                                    client_id: String::default(),
                                    role: AgentRole::Sink,
                                },
                            )) {
                                error!("Failed to send TestComplete to controller: {}", e);
                            }
                            info!("Sink agent finished, delaying stop to allow message delivery");
                            // Small delay to let the harness_client process the message
                            tokio::time::sleep(Duration::from_millis(100)).await;

                            info!("Stopping supervisor now");
                            myself.stop(Some("Sink finished".to_string()));
                            return Ok(());
                        } else {
                            // Abnormal termination – report error
                            let error_msg = format!("Sink agent terminated unexpectedly: {:?}", reason);
                            error!("{}", error_msg);
                            if let Err(e) = state.harness_client.send_message(ControlClientMsg::SendCommand(
                                ControllerCommand::TestError { error: error_msg },
                            )) {
                                error!("Failed to send TestError to controller: {}", e);
                            }
                            myself.stop(Some("Sink agent failed".to_string()));
                            return Ok(());
                        }
                    }
                }

                // Failsafe: if no children left, stop supervisor
                let children = myself.get_children();
                if children.is_empty() {
                    info!("No children left, stopping supervisor");
                    myself.stop(Some("All children terminated".to_string()));
                }
            }
            SupervisionEvent::ActorFailed(actor_cell, error) => {
                let error_msg = format!(
                    "Worker agent: {:?}:{:?} failed! {}",
                    actor_cell.get_name(),
                    actor_cell.get_id(),
                    error
                );
                error!("{}", error_msg);
                // Notify controller
                if let Err(e) = state.harness_client.send_message(ControlClientMsg::SendCommand(
                    ControllerCommand::TestError {
                        error: error_msg.clone(),
                    },
                )) {
                    error!("Failed to send TestError to controller: {}", e);
                }
                // Stop supervisor
                myself.stop(Some(error_msg));
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
                debug!("Controller connected, saying hello.");
                let cmd = ControllerCommand::Hello { role: AgentRole::Sink };
                if let Err(e) = state.harness_client.send_message(ControlClientMsg::SendCommand(cmd)) {
                    error!("Failed to send Hello to controller: {}", e);
                    // This is critical; stop supervisor
                    myself.stop(Some(format!("Failed to send Hello: {}", e)));
                }
            }
            SupervisorMessage::SinkReady => {
                debug!("Sink ready, notifying controller");
                let cmd = ControllerCommand::SinkReady;
                if let Err(e) = state.harness_client.send_message(ControlClientMsg::SendCommand(cmd)) {
                    error!("Failed to send SinkReady to controller: {}", e);
                }
            }
            SupervisorMessage::CommandReceived { command } => match command {
                ControllerCommand::SinkConfig { topic, expected_count } => {
                    // Stop existing sink actor if any
                    if let Some(old_sink) = state.sink.take() {
                        info!("Stopping previous sink actor (ID: {:?})", old_sink.get_id());
                        old_sink.stop(Some("NEW_SINK_CONFIG".to_string()));
                        // Wait up to 10 seconds for it to stop cleanly
                        match old_sink.wait(Some(Duration::from_secs(10))).await {
                            Ok(()) => info!("Previous sink stopped cleanly"),
                            Err(e) => {
                                error!("Previous sink did not stop within timeout: {:?}. Aborting test.", e);
                                // Notify controller of fatal error
                                if let Err(send_err) = state.harness_client.send_message(ControlClientMsg::SendCommand(
                                    ControllerCommand::TestError {
                                        error: format!("Previous sink did not stop: {:?}", e),
                                    },
                                )) {
                                    error!("Failed to send TestError to controller: {}", send_err);
                                }
                                // Stop supervisor – cannot proceed
                                myself.stop(Some("Previous sink failed to stop".to_string()));
                                return Ok(());
                            }
                        }
                    }

                    info!("Starting new sink actor for topic {} (expected: {:?})", topic, expected_count);
                    let args = harness_sink::sink::SinkConfig {
                        topic: topic.clone(),
                        expected_count,
                    };

                    // Generate a unique instance ID and include it in the actor's name
                    let instance_id = uuid::Uuid::new_v4().to_string();
                    let actor_name = format!("cassini.harness.sink.{}.{}", topic, instance_id);

                    match Actor::spawn_linked(
                        Some(actor_name),
                        harness_sink::sink::SinkAgent,
                        args,
                        myself.clone().into(),
                    )
                    .await
                    {
                        Ok((sink, _)) => {
                            state.sink = Some(sink);
                        }
                        Err(e) => {
                            error!("Failed to spawn sink actor: {}", e);
                            if let Err(send_err) = state.harness_client.send_message(ControlClientMsg::SendCommand(
                                ControllerCommand::TestError {
                                    error: format!("Failed to spawn sink actor: {}", e),
                                },
                            )) {
                                error!("Failed to send TestError to controller: {}", send_err);
                            }
                            myself.stop(Some("Sink spawn failed".to_string()));
                        }
                    }
                }
                ControllerCommand::ProducerFinished => {
                    debug!("Producer finished, sending graceful stop to sink...");
                    if let Some(sink) = state.sink.as_ref() {
                        if let Err(e) = sink.send_message(harness_sink::sink::SinkAgentMsg::GracefulStop) {
                            error!("Failed to send GracefulStop to sink: {}", e);
                        }
                    } else {
                        warn!("Received ProducerFinished but no sink actor exists");
                    }
                }
                ControllerCommand::Shutdown => {
                    debug!("Received shutdown command. stopping sink client and commencing validation.");
                    myself.stop(Some("SHUTDOWN".to_string()));
                }
                _ => {
                    warn!("Received unknown command {command:?}");
                }
            },
            SupervisorMessage::TransportError { reason } => {
                if state.sink.is_none() || state.work_completed {
                    info!("Controller disconnected after work completed (shutdown): {}", reason);
                } else {
                    error!("Lost connection to controller: {}", reason);
                    myself.stop(Some(reason));
                }
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
    init_tracing("cassini-harness-sink");

    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        eprintln!("\n========== SINK PANIC ==========");
        eprintln!("{}", info);
        if let Some(location) = info.location() {
            eprintln!("  at {}:{}", location.file(), location.line());
        }
        eprintln!("================================\n");
        default_hook(info);
        std::process::exit(1);
    }));

    info!("Sink agent starting up.");

    let (_, handle) = Actor::spawn(
        Some("cassini.harness.sink.supervisor".to_string()),
        SinkService,
        (),
    )
    .await
    .expect("Expected to start sink supervisor");

    // Wait indefinitely for the supervisor to stop (should happen after test completes)
    match handle.await {
        Ok(()) => info!("Sink supervisor stopped normally"),
        Err(e) => error!("Sink supervisor stopped with error: {}", e),
    }

    shutdown_tracing();
    std::process::exit(0);
}
