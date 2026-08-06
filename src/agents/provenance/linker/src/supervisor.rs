use crate::{
    PROVENANCE_LIKER_NAME,
    linker::{ProvenanceLinker, ProvenanceLinkerArgs},
};
use cassini_types::ClientEvent;
use cassini_types::WireTraceCtx;
use neo4rs::Graph;
use polar::cassini::SubscribeRequest;
use polar::graph::controller::{GraphControllerActor, GraphSignal};
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use polar::{
    ARTIFACT_PRODUCED_SUFFIX, BUILDS_TOPIC_PREFIX, BuildEvent, PROVENANCE_LINKER_TOPIC,
    ProvenanceEvent, SBOM_RESOLVED_SUFFIX, SupervisorMessage,
    cassini::{CassiniClient, TcpClient},
    get_neo_config,
};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};
use std::sync::Arc;
use tracing::{debug, info, warn};
use tracing::{error, instrument};

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

// === Supervisor state ===
pub struct ProvenanceSupervisorState {
    pub graph: Graph,
    pub broker_client: Arc<dyn CassiniClient>,
    pub linker: Option<ActorRef<ProvenanceEvent>>,
    pub healthcheck: ActorRef<HealthCheckMessage>,
    pub draining: bool,
}

// === Supervisor definition ===

pub struct ProvenanceSupervisor;

impl ProvenanceSupervisor {
    #[instrument(name = "ProvenanceSupervisor::deserialize_and_dispatch" skip(payload, state))]
    fn deserialize_and_dispatch(
        topic: String,
        payload: Vec<u8>,
        state: &mut ProvenanceSupervisorState,
    ) {
        debug!("Received message on topic {topic}");

        pub fn try_deserialize(payload: &[u8]) -> Option<ProvenanceEvent> {
            if let Ok(event) = rkyv::from_bytes::<ProvenanceEvent, rkyv::rancor::Error>(payload) {
                return Some(event);
            }

            match BuildEvent::from_bytes(payload) {
                Ok(e) => {
                    let (_ctx, event) = e.into_provenance_event();
                    return Some(event);
                }
                Err(e) => {
                    error!("Failed ot deserialize build event! {e}");
                }
            }
            None
        }

        if let Some(event) = try_deserialize(&payload) {
            state.linker.as_ref().map(|l| l.send_message(event));
        } else {
            warn!("Failed to deserialize provenance event!")
        }
    }
}
#[async_trait]
impl Actor for ProvenanceSupervisor {
    type Msg = SupervisorMessage;
    type State = ProvenanceSupervisorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        // --- NEW: healthcheck wiring ---
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
        // --- end NEW ---

        let graph = neo4rs::Graph::connect(get_neo_config()?)?;

        let broker_client = Arc::new(
            TcpClient::spawn(
                "polar.artifacts.linker.supervisor.tcp",
                myself.clone(),
                |event| Some(SupervisorMessage::ClientEvent { event }),
            )
            .await?,
        );

        let s = ProvenanceSupervisorState {
            graph,
            broker_client,
            linker: None,
            healthcheck,
            draining: false,
        };

        Ok(s)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} started.");
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
            // --- NEW ---
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
            // --- end NEW ---
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    // --- NEW ---
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);
                    // --- end NEW ---

                    debug!("Subscribing to topic {}", PROVENANCE_LINKER_TOPIC);
                    state.broker_client.subscribe(SubscribeRequest {
                        topic: PROVENANCE_LINKER_TOPIC.to_string(),
                        trace_ctx: WireTraceCtx::from_current_span(),
                    })?;

                    state.broker_client.subscribe(SubscribeRequest {
                        topic: format!("{BUILDS_TOPIC_PREFIX}.{ARTIFACT_PRODUCED_SUFFIX}"),
                        trace_ctx: WireTraceCtx::from_current_span(),
                    })?;
                    state.broker_client.subscribe(SubscribeRequest {
                        topic: format!("{BUILDS_TOPIC_PREFIX}.{SBOM_RESOLVED_SUFFIX}"),
                        trace_ctx: WireTraceCtx::from_current_span(),
                    })?;

                    state.broker_client.subscribe(SubscribeRequest {
                        topic: format!("{BUILDS_TOPIC_PREFIX}.binary.linked"),
                        trace_ctx: WireTraceCtx::from_current_span(),
                    })?;

                    let graph_signal_port = Arc::new(OutputPort::<GraphSignal>::default());
                    graph_signal_port.subscribe(myself.clone(), |signal| {
                        Some(SupervisorMessage::GraphSignal(signal))
                    });
                    let (compiler, _) = Actor::spawn_linked(
                        Some("linker.graph.controller".to_string()),
                        GraphControllerActor,
                        polar::graph::controller::GraphControllerArgs { signal_port: graph_signal_port },
                        myself.clone().into(),
                    )
                    .await?;
                    // --- NEW ---
                    let _ = state.healthcheck.cast(HealthCheckMessage::GraphConnected);
                    // --- end NEW ---

                    let linker_args = ProvenanceLinkerArgs { compiler };

                    let (linker, _) = Actor::spawn_linked(
                        Some(PROVENANCE_LIKER_NAME.to_string()),
                        ProvenanceLinker,
                        linker_args,
                        myself.clone().into(),
                    )
                    .await?;

                    state.linker = Some(linker);
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    // --- NEW: drain guard ---
                    if state.draining {
                        warn!(
                            "draining -- logging message on topic '{topic}' \
                             ({} bytes) instead of dispatching; will be lost on exit",
                            payload.len()
                        );
                    } else {
                        Self::deserialize_and_dispatch(topic, payload, state)
                    }
                    // --- end NEW ---
                }
                // BUG FIX: was `myself.stop(Some(reason))` -- silent exit-0,
                // Job marks Completed, no restart, no visible failure.
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniDisconnected);
                }

                _ => (),
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
            // BUG FIX: was `todo!("Implement some restart logic for the linker")`
            // -- a live panic on any child actor failure, including the
            // graph controller itself. Now routes through the same bounded
            // drain-and-exit path as every other agent.
            SupervisionEvent::ActorFailed(name, err) => {
                error!("Actor {name:?} failed! {err:?}");
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
            SupervisionEvent::ActorTerminated(name, _state, reason) => {
                let actor_name = name.get_name().unwrap_or_default();
                warn!("Actor {name:?} terminated! {reason:?}");
                // --- NEW ---
                if actor_name == HEALTHCHECK_ACTOR_NAME && !state.draining {
                    state.draining = true;
                    info!("entering drain window before exiting for rejuvenation");
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                } else if !state.draining {
                    // BUG FIX: was `myself.stop(reason)` unconditionally --
                    // silent exit-0 under Job for any non-healthcheck actor
                    // termination too (e.g. linker or graph controller
                    // stopping cleanly for some other reason).
                    state.draining = true;
                    warn!("entering drain window before exit");
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
                // --- end NEW ---
            }
            SupervisionEvent::ActorStarted(actor) => {
                debug!("Actor {actor:?} started!");
            }
            _ => {}
        }
        Ok(())
    }
}
