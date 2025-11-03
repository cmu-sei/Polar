use harness_common::{
    HarnessControllerMessage,
    client::{HarnessClient, HarnessClientArgs, HarnessClientConfig, HarnessClientMessage},
};
use harness_sink::sink::*;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};

use tokio_util::sync::CancellationToken;

use tokio_rustls::{TlsAcceptor, server::TlsStream};

use tracing::{debug, error, info, warn};

pub const SINK_CLIENT_SESSION: &str = "cassini.harness.sink.session";

// ============================== Sink Service ============================== //

pub struct SinkService;

pub struct SinkServiceState {
    harness_client: ActorRef<HarnessClientMessage>,
    timeout_token: CancellationToken,
}

pub struct SinkServiceArgs {
    tcp_client_config: HarnessClientConfig,
}

#[async_trait]
impl Actor for SinkService {
    type Msg = HarnessControllerMessage;
    type State = SinkServiceState;
    type Arguments = SinkServiceArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: SinkServiceArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!("Starting {myself:?}");
        tracing::info!("RootActor: Started {myself:?}");

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
            HarnessControllerMessage::TestPlan { plan } => {
                // abort timeout
                state.timeout_token.cancel();

                info!(
                    "Received test plan from client. {}",
                    serde_json::to_string_pretty(&plan).unwrap()
                );
                info!("Starting subscribes.");
                // start a sink for each producer, if we already have a sink subscriber for that topic, skip

                for config in plan.producers {
                    let args = SinkConfig {
                        topic: config.topic.clone(),
                    };

                    let _ = Actor::spawn_linked(
                        Some(format!("cassini.harness.sink.{}", config.topic)),
                        SinkAgent,
                        args,
                        myself.clone().into(),
                    )
                    .await
                    .map_err(|_e| ());
                }
            }
            HarnessControllerMessage::Shutdown => {
                info!("Shutdown command received. Shutting down.");

                // write back to the producer that we're stopping
                state
                    .harness_client
                    .cast(HarnessClientMessage::Send(
                        HarnessControllerMessage::ShutdownAck,
                    ))
                    .ok();

                myself.stop_children_and_wait(None, None).await;

                myself.stop(None);
            }
            _ => (),
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() {
    polar::init_logging();

    info!("Sink agent starting up.");

    let client_config = HarnessClientConfig::new();

    let (_, handle) = Actor::spawn(
        Some("cassini.harness.sink.supervisor".to_string()),
        SinkService,
        SinkServiceArgs {
            tcp_client_config: client_config,
        },
    )
    .await
    .expect("Expected to start sink server");

    handle.await.unwrap();
}
