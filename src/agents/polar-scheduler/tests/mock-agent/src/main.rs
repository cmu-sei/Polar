use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent;
use polar_scheduler_common::ScheduleNotification;
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort};
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tracing::{error, info};

const AGENT_ID: &str = "test-permanent";

// Actor that forwards ClientEvents into a channel
struct EventForwarder;

#[ractor::async_trait]
impl Actor for EventForwarder {
    type Msg = ClientEvent;
    type State = UnboundedSender<ClientEvent>;
    type Arguments = UnboundedSender<ClientEvent>;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        tx: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(tx)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let _ = state.send(msg);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    polar::init_logging("mock-agent".to_string());

    // Channel for receiving client events
    let (tx, mut rx) = unbounded_channel();

    // Spawn the event forwarder actor
    let (forwarder, _) = Actor::spawn(
        Some("event-forwarder".to_string()),
        EventForwarder,
        tx,
    )
    .await?;

    // OutputPort that will receive ClientEvents from the TcpClient
    let events_output: Arc<OutputPort<ClientEvent>> = Arc::new(OutputPort::default());
    events_output.subscribe(forwarder, |event| Some(event));

    let config = TCPClientConfig::new().expect("Failed to create TCP client config");
    let (tcp_client, _handle) = Actor::spawn(
        Some("mock-agent.tcp".to_string()),
        TcpClientActor,
        TcpClientArgs {
            config,
            registration_id: None,
            events_output: Some(events_output),
            event_handler: None,
        },
    )
    .await?;

    info!("Mock agent waiting for registration...");

    // Wait for registration complete
    let mut registered = false;
    while let Some(event) = rx.recv().await {
        match event {
            ClientEvent::Registered { .. } => {
                info!("Client registered, now subscribing to schedule topic");
                tcp_client.cast(TcpClientMessage::Subscribe {
                    topic: format!("agent.{}.schedule", AGENT_ID),
                    trace_ctx: None,
                })?;
                registered = true;
                break;
            }
            _ => {}
        }
    }

    if !registered {
        error!("Failed to register with broker");
        return Ok(());
    }

    // // Connect to Neo4j
    // let graph = {
    //     let uri = std::env::var("GRAPH_ENDPOINT").expect("GRAPH_ENDPOINT not set");
    //     let user = std::env::var("GRAPH_USER").expect("GRAPH_USER not set");
    //     let password = std::env::var("GRAPH_PASSWORD").expect("GRAPH_PASSWORD not set");
    //     let db = std::env::var("GRAPH_DB").unwrap_or_else(|_| "neo4j".to_string());

    //     let config = neo4rs::ConfigBuilder::default()
    //         .uri(uri)
    //         .user(user)
    //         .password(password)
    //         .db(db)
    //         .build()
    //         .expect("Failed to build Neo4j config");
    //     Graph::connect(config).expect("Failed to connect to Neo4j")
    // };

    info!("Mock agent ready, waiting for notifications...");

    while let Some(event) = rx.recv().await {
        match event {
            ClientEvent::MessagePublished { topic, payload, .. } => {
                info!("Received message on topic: {}", topic);
                if topic == format!("agent.{}.schedule", AGENT_ID) {
                    match rkyv::from_bytes::<ScheduleNotification, rkyv::rancor::Error>(&payload) {
                        Ok(notif) => {
                            info!("Received notification: {:?}", notif);
                            if let ScheduleNotification::PermanentUpdate { schedule_json, .. } = notif {
                                // // Fetch full schedule from graph
                                // let mut result = graph
                                //     .execute(
                                //         neo4rs::query(
                                //             "MATCH (s:Schedule:Permanent {agent_id: $agent_id}) RETURN s.schedule as schedule"
                                //         )
                                //         .param("agent_id", agent_id.clone()),
                                //     )
                                //     .await?;
                                // if let Some(row) = result.next().await? {
                                //   let schedule_json: String = row.get("schedule")?;
                                //    info!("Full schedule: {}", schedule_json);
                                // } else {
                                //     warn!("No schedule found for agent {}", agent_id);
                                // }
                                let _schedule: serde_json::Value = serde_json::from_str(&schedule_json)?;
                            }
                        }
                        Err(e) => error!("Failed to deserialize notification: {:?}", e),
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}
