use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent;
use polar_scheduler_common::{AdhocAgentAnnouncement, ScheduleNotification};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, info};

const AGENT_TYPE: &str = "test-adhoc";

// EventForwarder actor (same as before)
struct EventForwarder {
    tx: UnboundedSender<ClientEvent>,
}

impl EventForwarder {
    fn new(tx: UnboundedSender<ClientEvent>) -> Self {
        Self { tx }
    }
}

#[ractor::async_trait]
impl Actor for EventForwarder {
    type Msg = ClientEvent;
    type State = UnboundedSender<ClientEvent>;
    type Arguments = UnboundedSender<ClientEvent>;

    async fn pre_start(&self, _: ActorRef<Self::Msg>, tx: Self::Arguments) -> Result<Self::State, ActorProcessingErr> {
        Ok(tx)
    }

    async fn handle(&self, _: ActorRef<Self::Msg>, msg: Self::Msg, state: &mut Self::State) -> Result<(), ActorProcessingErr> {
        let _ = state.send(msg);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    polar::init_logging("mock-adhoc-agent".to_string());

    // Build a default schedule (hardcoded JSON for simplicity)
    let default_schedule_json = serde_json::json!({
        "id": format!("adhoc/{}", AGENT_TYPE),
        "kind": "Adhoc",
        "agent_id": null,
        "agent_type": AGENT_TYPE,
        "schedule": {
            "Periodic": { "interval": 60, "unit": "Seconds" }
        },
        "config": { "dummy": "default" },
        "metadata": {
            "description": "Mock ad-hoc agent",
            "owner": "test"
        },
        "version": 1
    }).to_string();

    let announcement = AdhocAgentAnnouncement {
        agent_type: AGENT_TYPE.to_string(),
        version: 1,
        default_schedule_json,
    };

    // Channel for receiving client events
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    // Spawn event forwarder
    let (forwarder, _) = Actor::spawn(
        Some("event-forwarder".to_string()),
        EventForwarder::new(tx.clone()),
        tx,
    )
    .await?;

    let events_output: Arc<OutputPort<ClientEvent>> = Arc::new(OutputPort::default());
    events_output.subscribe(forwarder, |event| Some(event));

    let config = TCPClientConfig::new().expect("Failed to create TCP client config");
    let (tcp_client, _handle) = Actor::spawn(
        Some("mock-adhoc-agent.tcp".to_string()),
        TcpClientActor,
        TcpClientArgs {
            config,
            registration_id: None,
            events_output: Some(events_output),
            event_handler: None,
        },
    )
    .await?;

    info!("Mock ad-hoc agent waiting for registration...");

    // Wait for registration
    let mut registered = false;
    while let Some(event) = rx.recv().await {
        if let ClientEvent::Registered { .. } = event {
            info!("Registered, subscribing to type topic and sending announcement");
            tcp_client.cast(TcpClientMessage::Subscribe {
                topic: format!("agent.type.{}.schedule", AGENT_TYPE),
                trace_ctx: None,
            })?;
            // Send announcement (formerly registration)
            let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&announcement)?.to_vec();
            tcp_client.cast(TcpClientMessage::Publish {
                topic: "scheduler.adhoc".to_string(),
                payload,
                trace_ctx: None,
            })?;
            registered = true;
            break;
        }
    }

    if !registered {
        error!("Failed to register with Cassini");
        return Ok(());
    }

    info!("Mock ad-hoc agent ready, waiting for schedule updates...");

    while let Some(event) = rx.recv().await {
        if let ClientEvent::MessagePublished { topic, payload, .. } = event {
            if topic == format!("agent.type.{}.schedule", AGENT_TYPE) {
                match rkyv::from_bytes::<ScheduleNotification, rkyv::rancor::Error>(&payload) {
                    Ok(notif) => {
                        info!("Received notification: {:?}", notif);
                        if let ScheduleNotification::AdhocUpdate { agent_type, schedule_json } = notif {
                            info!("Ad-hoc agent {} received schedule: {}", agent_type, schedule_json);
                        }
                    }
                    Err(e) => error!("Failed to deserialize notification: {:?}", e),
                }
            }
        }
    }

    Ok(())
}
