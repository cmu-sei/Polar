use crate::processor::ScheduleInfoProcessor;
use crate::types::ProcessorMsg;
use polar_scheduler_common::{GitScheduleChange, AdhocAgentAnnouncement};
use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent;
use polar::SupervisorMessage;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent};
use tracing::{debug, error, warn, info};

pub const SERVICE_NAME: &str = "polar.scheduler";

pub struct RootSupervisor;

pub struct RootSupervisorState {
    tcp_client: ActorRef<TcpClientMessage>,
    processor: Option<ActorRef<ProcessorMsg>>,
}

#[async_trait]
impl Actor for RootSupervisor {
    type Msg = SupervisorMessage;
    type State = RootSupervisorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("RootSupervisor starting");

        let events_output = std::sync::Arc::new(OutputPort::default());
        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        let config = TCPClientConfig::new()?;
        let (tcp_client, _) = Actor::spawn_linked(
            Some(format!("{}.tcp", SERVICE_NAME)),
            TcpClientActor,
            TcpClientArgs {
                config,
                registration_id: None,
                events_output: Some(events_output),
                event_handler: None,
            },
            myself.clone().into(),
        )
        .await?;

        Ok(RootSupervisorState {
            tcp_client,
            processor: None,
        })
    }

    async fn post_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,  // prefix with underscore
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
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let neo_config = {
                        let uri = std::env::var("GRAPH_ENDPOINT")
                            .map_err(|_| ActorProcessingErr::from("GRAPH_ENDPOINT not set"))?;
                        let user = std::env::var("GRAPH_USER")
                            .map_err(|_| ActorProcessingErr::from("GRAPH_USER not set"))?;
                        let password = std::env::var("GRAPH_PASSWORD")
                            .map_err(|_| ActorProcessingErr::from("GRAPH_PASSWORD not set"))?;
                        let db = std::env::var("GRAPH_DB").unwrap_or_else(|_| "neo4j".to_string());

                        let mut builder = neo4rs::ConfigBuilder::default()
                            .uri(uri)
                            .user(user)
                            .password(password)
                            .db(db)
                            .fetch_size(500)
                            .max_connections(10);

                        if let Ok(cert) = std::env::var("GRAPH_CA_CERT") {
                            builder = builder.with_client_certificate(cert);
                        }

                        builder.build().expect("Failed to build Neo4j config")
                    };

                    match neo4rs::Graph::connect(neo_config) {
                        Ok(graph) => {
                            let (processor, _) = Actor::spawn_linked(
                                Some(format!("{}.processor", SERVICE_NAME)),
                                ScheduleInfoProcessor,
                                (state.tcp_client.clone(), graph),
                                myself.clone().into(),
                            )
                            .await?;
                            state.processor = Some(processor);

                            state.tcp_client.cast(TcpClientMessage::Subscribe {
                                topic: "scheduler.in".to_string(),
                                trace_ctx: None,
                            })?;
                            state.tcp_client.cast(TcpClientMessage::Subscribe {
                                topic: "scheduler.adhoc".to_string(),
                                trace_ctx: None,
                            })?;
                            state.tcp_client.cast(TcpClientMessage::Subscribe {
                                topic: "events.#".to_string(),
                                trace_ctx: None,
                            })?;
                        }
                        Err(e) => {
                            error!("Failed to connect to Neo4j: {:?}", e);
                            myself.stop(Some(format!("Neo4j connection failed: {}", e)));
                        }
                    }
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    if let Some(p) = &state.processor {
                        info!("Received message on topic: {}", topic);
                        if topic == "scheduler.in" {
                            match rkyv::from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&payload) {
                                Ok(change) => {
                                    info!("Deserialized GitScheduleChange, forwarding to processor");
                                    if let Err(e) = p.cast(ProcessorMsg::GitChange(change)) {
                                        error!("Failed to cast GitChange to processor: {:?}", e);
                                    }
                                }
                                Err(e) => error!("Failed to deserialize GitScheduleChange: {:?}", e),
                            }
                        } else if topic == "scheduler.adhoc" {   // Changed topic name to avoid "registration"
                            match rkyv::from_bytes::<AdhocAgentAnnouncement, rkyv::rancor::Error>(&payload) {
                                Ok(ann) => {
                                    info!("Deserialized ad-hoc agent announcement, forwarding");
                                    if let Err(e) = p.cast(ProcessorMsg::Announcement(ann)) {
                                        error!("Failed to cast announcement: {:?}", e);
                                    }
                                }
                                Err(e) => error!("Failed to deserialize ad-hoc announcement: {:?}", e),
                            }
                        } else if topic.starts_with("events.") {
                            info!("Received event on topic: {}", topic);
                            p.cast(ProcessorMsg::Event { topic, payload })?;
                        }
                    } else {
                        warn!("Message received but processor not yet spawned");
                    }
                }
                ClientEvent::TransportError { reason } => {
                    error!("Transport error: {}", reason);
                    myself.stop(Some(reason));
                }
                _ => {}
            },
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorFailed(actor_cell, err) => {
                let name_str = actor_cell.get_name().unwrap_or_else(|| "unknown".to_string());
                error!("Actor {} failed: {:?}", name_str, err);
            }
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                let name_str = actor_cell.get_name().unwrap_or_else(|| "unknown".to_string());
                warn!("Actor {} terminated: {:?}", name_str, reason);
            }
            _ => {}
        }
        Ok(())
    }
}
