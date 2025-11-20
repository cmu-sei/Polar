use std::time::Duration;

use crate::linker::{ProvenanceLinker, ProvenanceLinkerArgs};
use neo4rs::Graph;
use polar::get_neo_config;
use provenance_common::PROVENANCE_LINKER_NAME;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use tracing::debug;

// === Messages ===
pub enum ProvenanceSupervisorMsg {
    RestartChild(String),
    Stop,
}
// === Supervisor state ===
pub struct ProvenanceSupervisorState {
    pub graph: Graph,
}

// === Supervisor definition ===

pub struct ProvenanceSupervisor;

#[async_trait]
impl Actor for ProvenanceSupervisor {
    type Msg = ProvenanceSupervisorMsg;
    type State = ProvenanceSupervisorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        // get graph connection
        //TODO: Connect to cassini and await some some schedule from configuration agent
        //
        match neo4rs::Graph::connect(get_neo_config()) {
            Ok(graph) => Ok(ProvenanceSupervisorState { graph }),
            Err(e) => Err(ActorProcessingErr::from(e)),
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} started.");

        let linker_args = ProvenanceLinkerArgs {
            graph: state.graph.clone(),
            interval: Duration::from_secs(30),
        };

        let _ = Actor::spawn_linked(
            Some(PROVENANCE_LINKER_NAME.to_string()),
            ProvenanceLinker,
            linker_args,
            myself.clone().into(),
        )
        .await
        .expect("Failed to start actor!");

        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        _msg: ProvenanceSupervisorMsg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorFailed(name, _reason) => {
                debug!("Actor {name:?} failed! {_reason:?}");
                myself.stop(None);
            }
            _ => {}
        }
        Ok(())
    }
}
