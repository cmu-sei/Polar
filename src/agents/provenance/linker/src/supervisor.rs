use crate::BROKER_CLIENT_NAME;
use crate::{
    linker::{ProvenanceLinker, ProvenanceLinkerArgs},
    PROVENANCE_LIKER_NAME,
};
use cassini_client::{TCPClientConfig, TcpClientArgs};
use cassini_types::ClientEvent;
use neo4rs::Graph;
use polar::{
    get_neo_config, ProvenanceEvent, Supervisor, SupervisorMessage, PROVENANCE_LINKER_TOPIC,
};
use ractor::{
    async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef, OutputPort,
    SupervisionEvent,
};
use std::sync::Arc;
use tracing::{debug, warn};
use tracing::{error, instrument, trace};
use cassini_types::WireTraceCtx;

// === Supervisor state ===
pub struct ProvenanceSupervisorState {
    pub graph: Graph,
    pub broker_client: ActorRef<cassini_client::TcpClientMessage>,
    pub events_output: Arc<OutputPort<ClientEvent>>,
}

// === Supervisor definition ===

pub struct ProvenanceSupervisor;

impl Supervisor for ProvenanceSupervisor {
    #[instrument(name = "ProvenanceSupervisor::deserialize_and_dispatch" skip(payload))]
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>) {
        debug!("Received message on topic {topic}");
        match rkyv::from_bytes::<ProvenanceEvent, rkyv::rancor::Error>(&payload) {
            Ok(event) => {
                //lookup linker and forward
                trace!("looking up actor {PROVENANCE_LINKER_TOPIC} and forwarding");
                if let Some(linker) = where_is(PROVENANCE_LINKER_TOPIC.to_string()) {
                    linker
                        .send_message(event)
                        .map_err(|e| error!("Failed to forward event to linker! {e}"))
                        .ok();
                }
            }
            Err(e) => warn!("Failed to parse provenance event. {e}"),
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

        let events_output = Arc::new(OutputPort::default());

        events_output.subscribe(myself.clone(), |event| {
            debug!("Received event: {event:?}");
            Some(SupervisorMessage::ClientEvent { event })
        });
        let config = TCPClientConfig::new()?;  // create config (adjust error handling as needed)
        let client_args = TcpClientArgs {
            config,
            registration_id: None,
            events_output: Some(events_output.clone()),
            event_handler: None,
        };

        let (broker_client, _) = Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            cassini_client::TcpClientActor,
            client_args,
            myself.clone().into(),
        )
        .await?;

        let s = ProvenanceSupervisorState {
            graph,
            broker_client,
            events_output: events_output.clone(),
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
                    state
                        .broker_client
                        .cast(cassini_client::TcpClientMessage::Subscribe {
                            topic: PROVENANCE_LINKER_TOPIC.to_string(),
                            trace_ctx: WireTraceCtx::from_current_span(),
                        })?;

                    let graph = neo4rs::Graph::connect(get_neo_config()?)?;

                    let (compiler, _) = Actor::spawn_linked(
                        Some("linker.graph.controller".to_string()),
                        crate::LinkerGraphController,
                        graph,
                        myself.clone().into(),
                    )
                    .await?;

                    let linker_args = ProvenanceLinkerArgs { compiler };

                    let (_linker, _) = Actor::spawn_linked(
                        Some(PROVENANCE_LIKER_NAME.to_string()),
                        ProvenanceLinker,
                        linker_args,
                        myself.clone().into(),
                    )
                    .await?;
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    Self::deserialize_and_dispatch(topic, payload)
                }
                ClientEvent::TransportError { reason } => {
                    error!("Transport error: {reason}");
                    myself.stop(Some(reason))
                }
                ClientEvent::ControlResponse { .. } => {
                    // ignore or log
                    debug!("Ignoring ControlResponse in linker");
                }
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
