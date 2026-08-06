use crate::processor::ScheduleInfoProcessor;
use crate::types::ProcessorMsg;
use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent;
use polar::SupervisorMessage;
use polar::graph::controller::{GraphControllerActor, GraphSignal};
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use polar_scheduler_common::{AdhocAgentAnnouncement, GitScheduleChange};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

pub const SERVICE_NAME: &str = "polar.scheduler";
const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

pub struct RootSupervisor;

pub struct RootSupervisorState {
    tcp_client: ActorRef<TcpClientMessage>,
    processor: Option<ActorRef<ProcessorMsg>>,
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
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

        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        let dep_cert_endpoints = vec![
            DepCertEndpoint::parse("cassini-ip-svc.polar.svc.cluster.local:8080:300")
                .map_err(|e| ActorProcessingErr::from(e))?,
            DepCertEndpoint::parse("polar-db-svc.polar-graph.svc.cluster.local:7687:300")
                .map_err(|e| ActorProcessingErr::from(e))?,
        ];

        let (healthcheck, _) = HealthCheckActor::spawn_linked(
            Some(HEALTHCHECK_ACTOR_NAME.to_string()),
            HealthCheckActor,
            HealthCheckArgs {
                expects_graph: true,
                rejuvenation_threshold_secs: 300,
                dep_cert_endpoints,
                prepare_shutdown_port,
            },
            myself.get_cell(),
        )
        .await
        .map_err(|e| ActorProcessingErr::from(e))?;

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
            healthcheck,
            draining: false,
        })
    }

    async fn post_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
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
            SupervisorMessage::Heartbeat => {}
            SupervisorMessage::PrepareShutdown => {
                info!("PrepareShutdown received");
                if let Err(e) = state.healthcheck.cast(HealthCheckMessage::ShutdownAck) {
                    error!("failed to send ShutdownAck: {e}");
                }
            }
            SupervisorMessage::GraphSignal(signal) => match signal {
                GraphSignal::Available => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::GraphAvailable);
                }
                GraphSignal::OpFailed(reason) => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::GraphOpFailed(reason));
                }
            },
            SupervisorMessage::ForceExit => {
                warn!("drain window elapsed, exiting now");
                std::process::exit(1);
            }
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);
                    let graph_signal_port = std::sync::Arc::new(ractor::OutputPort::<GraphSignal>::default());
                    graph_signal_port.subscribe(myself.clone(), |signal| {
                        Some(SupervisorMessage::GraphSignal(signal))
                    });
                    let (graph_controller, _) = GraphControllerActor::spawn_linked(
                        Some(format!("{}.graph_controller", SERVICE_NAME)),
                        GraphControllerActor,
                        polar::graph::controller::GraphControllerArgs { signal_port: graph_signal_port },
                        myself.get_cell(),
                    )
                    .await?;
                    let _ = state.healthcheck.cast(HealthCheckMessage::GraphConnected);

                    let (processor, _) = Actor::spawn_linked(
                        Some(format!("{}.processor", SERVICE_NAME)),
                        ScheduleInfoProcessor,
                        (state.tcp_client.clone(), graph_controller),
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
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    if state.draining {
                        warn!(
                            "draining -- logging message on topic '{topic}' \
                             ({} bytes) instead of dispatching; will be lost on exit",
                            payload.len()
                        );
                        return Ok(());
                    }
                    if let Some(p) = &state.processor {
                        info!("Received message on topic: {}", topic);
                        if topic == "scheduler.in" {
                            match rkyv::from_bytes::<GitScheduleChange, rkyv::rancor::Error>(
                                &payload,
                            ) {
                                Ok(change) => {
                                    info!(
                                        "Deserialized GitScheduleChange, forwarding to processor"
                                    );
                                    if let Err(e) = p.cast(ProcessorMsg::GitChange(change)) {
                                        error!("Failed to cast GitChange to processor: {:?}", e);
                                    }
                                }
                                Err(e) => {
                                    error!("Failed to deserialize GitScheduleChange: {:?}", e)
                                }
                            }
                        } else if topic == "scheduler.adhoc" {
                            match rkyv::from_bytes::<AdhocAgentAnnouncement, rkyv::rancor::Error>(
                                &payload,
                            ) {
                                Ok(ann) => {
                                    info!("Deserialized ad-hoc agent announcement, forwarding");
                                    if let Err(e) = p.cast(ProcessorMsg::Announcement(ann)) {
                                        error!("Failed to cast announcement: {:?}", e);
                                    }
                                }
                                Err(e) => {
                                    error!("Failed to deserialize ad-hoc announcement: {:?}", e)
                                }
                            }
                        } else if topic.starts_with("events.") {
                            info!("Received event on topic: {}", topic);
                            p.cast(ProcessorMsg::Event { topic, payload })?;
                        }
                    } else {
                        warn!("Message received but processor not yet spawned");
                    }
                }
                // BUG FIX: was `myself.stop(Some(reason))` -- silent exit-0,
                // Job marks Completed, no restart, no visible failure.
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniDisconnected);
                }
                _ => {}
            },
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorFailed(actor_cell, err) => {
                let name_str = actor_cell
                    .get_name()
                    .unwrap_or_else(|| "unknown".to_string());
                error!("Actor {} failed: {:?}", name_str, err);
                // BUG FIX: previously logged only, no exit trigger at all.
                if !state.draining {
                    state.draining = true;
                    warn!("entering drain window before exit");
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                let name_str = actor_cell
                    .get_name()
                    .unwrap_or_else(|| "unknown".to_string());
                warn!("Actor {} terminated: {:?}", name_str, reason);
                if name_str == HEALTHCHECK_ACTOR_NAME && !state.draining {
                    state.draining = true;
                    info!("entering drain window before exiting for rejuvenation");
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
