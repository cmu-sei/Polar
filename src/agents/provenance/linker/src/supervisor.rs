use crate::{
    PROVENANCE_LIKER_NAME,
    linker::{ProvenanceLinker, ProvenanceLinkerArgs},
};
use cassini_types::ClientEvent;
use cassini_types::WireTraceCtx;
use neo4rs::Graph;
use polar::cassini::SubscribeRequest;
use polar::graph::controller::GraphControllerActor;
use polar::{
    ARTIFACT_PRODUCED_SUFFIX, BUILDS_TOPIC_PREFIX, BuildEvent, PROVENANCE_LINKER_TOPIC,
    ProvenanceEvent, SBOM_RESOLVED_SUFFIX, SupervisorMessage,
    cassini::{CassiniClient, TcpClient},
    get_neo_config,
};
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use std::sync::Arc;
use tracing::{debug, warn};
use tracing::{error, instrument};

// === Supervisor state ===
pub struct ProvenanceSupervisorState {
    pub graph: Graph,
    pub broker_client: Arc<dyn CassiniClient>,
    pub linker: Option<ActorRef<ProvenanceEvent>>,
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

        /// ---------------------------------------------------------------------------
        /// rkyv is the hot path (Rust agents). JSON via BuildEvent is the cold
        /// path (nushell stages). The topic is available for logging/metrics but
        /// is NOT used as a deserialization discriminant — serde's tagged enum
        /// handles that.
        /// ---------------------------------------------------------------------------
        pub fn try_deserialize(payload: &[u8]) -> Option<ProvenanceEvent> {
            // Hot path: Rust agents serialize ProvenanceEvent directly with rkyv.
            // Zero-copy deserialize — no allocation, no JSON parsing overhead.
            if let Ok(event) = rkyv::from_bytes::<ProvenanceEvent, rkyv::rancor::Error>(payload) {
                return Some(event);
            }

            // Cold path: nushell pipeline stages emit JSON-wrapped BuildEvents.
            // Parse JSON, then map the typed payload into the domain enum.
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
        // get graph connection

        let graph = neo4rs::Graph::connect(get_neo_config()?)?;

        // spawn a cassini client
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
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    // subscribe to topic
                    //
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

                    let graph = neo4rs::Graph::connect(get_neo_config()?)?;

                    let (compiler, _) = Actor::spawn_linked(
                        Some("linker.graph.controller".to_string()),
                        GraphControllerActor,
                        graph,
                        myself.clone().into(),
                    )
                    .await?;

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
                    Self::deserialize_and_dispatch(topic, payload, state)
                }
                ClientEvent::TransportError { reason } => {
                    error!("Transport error: {reason}");
                    myself.stop(Some(reason))
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
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorFailed(name, err) => {
                error!("Actor {name:?} failed! {err:?}");
                // TODO: The only condition for crash or failure here should be if the DB goes down, in which case,
                // we should consider how much we care about dropping messages.
                todo!("Implement some restart logic for the linker");
            }
            SupervisionEvent::ActorTerminated(name, _state, reason) => {
                error!("Actor {name:?} failed! {reason:?}");
                myself.stop(reason)
            }
            SupervisionEvent::ActorStarted(actor) => {
                debug!("Actor {actor:?} started!");
            }
            _ => {}
        }
        Ok(())
    }
}
