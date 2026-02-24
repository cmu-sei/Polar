use cassini_client::{
    ClientEventForwarder, ClientEventForwarderArgs, TCPClientConfig, TcpClientActor,
    TcpClientArgs, TcpClientMessage,
};
use cassini_types::{ClientEvent, ControlOp, ControlResult};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::env;
use std::time::Duration;
use tracing::{error, info, warn};

/// Simple actor that just waits for registration and then sends shutdown command.
struct ShutdownController;

enum ShutdownMsg {
    CassiniEvent(ClientEvent),
}

#[ractor::async_trait]
impl Actor for ShutdownController {
    type Msg = ShutdownMsg;
    type State = Option<ActorRef<TcpClientMessage>>;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        // Create a forwarder to convert ClientEvent to our own message type.
        let (forwarder, _) = Actor::spawn_linked(
            Some("shutdown-forwarder".to_string()),
            ClientEventForwarder::new(),
            ClientEventForwarderArgs {
                target: myself.clone(),
                mapper: Box::new(|event| Some(ShutdownMsg::CassiniEvent(event))),
            },
            myself.clone().into(),
        )
        .await?;

        // Build TCP client config from environment.
        let config = TCPClientConfig::new()?;

        let tcp_args = TcpClientArgs {
            config,
            registration_id: None,
            events_output: None,
            event_handler: Some(forwarder.into()),
        };

        let (client, _) = Actor::spawn_linked(
            Some("shutdown-tcp-client".to_string()),
            TcpClientActor,
            tcp_args,
            myself.clone().into(),
        )
        .await?;

        Ok(Some(client))
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            ShutdownMsg::CassiniEvent(event) => match event {
                ClientEvent::Registered { registration_id } => {
                    info!("Registered with broker: {}", registration_id);
                    if let Some(client) = state.as_ref() {
                        // Read shutdown token from environment.
                        let auth_token = env::var("BROKER_SHUTDOWN_TOKEN").unwrap_or_default();
                        info!("Sending shutdown command with token: {}", auth_token);
                        let cmd = TcpClientMessage::ControlRequest {
                            op: ControlOp::PrepareForShutdown { auth_token },
                            trace_ctx: None,
                        };
                        if let Err(e) = client.send_message(cmd) {
                            error!("Failed to send shutdown command: {}", e);
                            myself.stop(Some("send_failed".to_string()));
                        }
                    }
                }
                ClientEvent::ControlResponse { result, .. } => {
                    match result {
                        Ok(ControlResult::ShutdownInitiated) => {
                            info!("Broker acknowledged shutdown. Exiting.");
                            // Give the broker a moment to start shutting down, then exit.
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            myself.stop(Some("shutdown_ack".to_string()));
                        }
                        Ok(other) => {
                            warn!("Unexpected control response: {:?}", other);
                            myself.stop(Some("unexpected_response".to_string()));
                        }
                        Err(e) => {
                            error!("Shutdown command failed: {:?}", e);
                            myself.stop(Some("shutdown_failed".to_string()));
                        }
                    }
                }
                ClientEvent::TransportError { reason } => {
                    error!("Broker connection lost: {}", reason);
                    myself.stop(Some("transport_error".to_string()));
                }
                _ => {} // ignore other events
            },
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing (optional but helpful)
    cassini_client::init_tracing("shutdown-tool");

    info!("Starting shutdown tool...");

    let (controller, handle) = Actor::spawn(
        Some("shutdown-controller".to_string()),
        ShutdownController,
        (),
    )
    .await?;

    // Wait for the controller to finish (it will stop itself after sending/acknowledging shutdown).
    handle.await?;

    cassini_client::shutdown_tracing();
    info!("Shutdown tool finished.");
    Ok(())
}
