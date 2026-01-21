use crate::BROKER_CLIENT_NAME;
use crate::{
    linker::{LinkerCommand, ProvenanceLinker, ProvenanceLinkerArgs},
    PROVENANCE_LIKER_NAME,
};
use cassini_client::{TCPClientConfig, TcpClientArgs};
use cassini_types::ClientEvent;
use neo4rs::Graph;
use polar::{
    get_neo_config, ProvenanceEvent, Supervisor, SupervisorMessage, PROVENANCE_LINKER_TOPIC,
};
use provenance_common::PROVENANCE_LINKER_NAME;
use ractor::{
    async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef, OutputPort,
    SupervisionEvent,
};
use std::sync::Arc;
use std::time::Duration;
use tracing::error;
use tracing::{debug, warn};

// === Supervisor state ===
pub struct ProvenanceSupervisorState {
    pub graph: Graph,
    pub broker_client: ActorRef<cassini_client::TcpClientMessage>,
    pub events_output: Arc<OutputPort<ClientEvent>>,
}

// === Supervisor definition ===

pub struct ProvenanceSupervisor;

impl Supervisor for ProvenanceSupervisor {
    fn deserialize_and_dispatch(_topic: String, payload: Vec<u8>) {
        match rkyv::from_bytes::<ProvenanceEvent, rkyv::rancor::Error>(&payload) {
            Ok(event) => {
                match event {
                    ProvenanceEvent::ImageRefResolved {
                        id,
                        uri,
                        digest,
                        media_type,
                    } => {
                        //lookup linker and forward
                        if let Some(linker) = where_is(PROVENANCE_LINKER_NAME.to_string()) {
                            linker
                                .send_message(LinkerCommand::LinkContainerImages {
                                    id,
                                    uri,
                                    digest,
                                    media_type,
                                })
                                .map_err(|e| error!("Failed to forward command to linker! {e}"))
                                .ok();
                        }
                    }
                    _ => todo!(),
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
            Some(SupervisorMessage::ClientEvent { event })
        });
        let client_config = TCPClientConfig::new()?;
        // start client
        let client_args = TcpClientArgs {
            config: client_config,
            registration_id: None,
            events_output: events_output.clone(),
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
        state: &mut Self::State,
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
                    state
                        .broker_client
                        .cast(cassini_client::TcpClientMessage::Subscribe(
                            PROVENANCE_LINKER_TOPIC.to_string(),
                        ))?;

                    let linker_args = ProvenanceLinkerArgs {
                        graph: state.graph.clone(),
                        interval: Duration::from_secs(30),
                    };

                    let (_linker, _) = Actor::spawn_linked(
                        Some(PROVENANCE_LIKER_NAME.to_string()),
                        ProvenanceLinker,
                        linker_args,
                        myself.clone().into(),
                    )
                    .await?;
                }
                _ => todo!(),
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
                myself.stop(Some(format!("{err:?}")));
            }
            SupervisionEvent::ActorTerminated(name, state, reason) => {
                error!("Actor {name:?} failed! {reason:?}");
                myself.stop(reason)
            }
            _ => {}
        }
        Ok(())
    }
}
