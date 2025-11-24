use std::process::Output;
use std::time::Duration;
use std::{collections::HashMap, sync::Arc};

use crate::{
    linker::{ProvenanceLinker, ProvenanceLinkerArgs},
    PROVENANCE_LIKER_NAME,
};
use cassini_client::{TCPClientConfig, TcpClientArgs};
use neo4rs::Graph;
use polar::{get_neo_config, QueueOutput};
use ractor::port::output;
use ractor::{
    async_trait, Actor, ActorProcessingErr, ActorRef, ActorStatus, OutputPort, SupervisionEvent,
};
use tracing::{debug, info, warn};

// === Messages ===
pub enum ProvenanceSupervisorMsg {
    Stop,
    Init,
}
// === Supervisor state ===
pub struct ProvenanceSupervisorState {
    pub graph: Graph,
    pub broker_client: ActorRef<cassini_client::TcpClientMessage>,
    pub queue_output: QueueOutput,
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

        let graph = neo4rs::Graph::connect(get_neo_config()?)?;

        let events_output = Arc::new(OutputPort::default());
        let queue_output = Arc::new(OutputPort::default());

        events_output.subscribe(myself.clone(), |_id| Some(ProvenanceSupervisorMsg::Init));
        let client_config = TCPClientConfig::new()?;
        // start client
        let client_args = TcpClientArgs {
            config: client_config,
            registration_id: None,
            events_output,
            queue_output: queue_output.clone(),
        };

        let (broker_client, _) = Actor::spawn_linked(
            Some("provenance.linker.tcp".to_string()),
            cassini_client::TcpClientActor,
            client_args,
            myself.clone().into(),
        )
        .await?;

        let s = ProvenanceSupervisorState {
            graph,
            broker_client,
            queue_output,
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
        msg: ProvenanceSupervisorMsg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            ProvenanceSupervisorMsg::Init => {
                let linker_args = ProvenanceLinkerArgs {
                    graph: state.graph.clone(),
                    interval: Duration::from_secs(30),
                };

                let (linker, _) = Actor::spawn_linked(
                    Some(PROVENANCE_LIKER_NAME.to_string()),
                    ProvenanceLinker,
                    linker_args,
                    myself.clone().into(),
                )
                .await?;

                // topic is known, there is only one consumer here, so we don't use it
                state.queue_output.subscribe(linker, |(payload, _topic)| {
                    ProvenanceLinker::handle_message(payload)
                });
            }
            _ => todo!(),
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
            SupervisionEvent::ActorFailed(name, _reason) => {
                debug!("Actor {name:?} failed! {_reason:?}");
                myself.stop(None);
            }
            _ => {}
        }
        Ok(())
    }
}
