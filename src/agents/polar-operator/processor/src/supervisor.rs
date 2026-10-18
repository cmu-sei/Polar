//! The processor's actor tree: a Cassini client and the graph controller.
//!
//! ```text
//! AgentProcessorSupervisor
//!   ├── TcpClient             (subscribes to polar.agents.lifecycle)
//!   └── GraphControllerActor  (serializes every Neo4j write)
//! ```
//!
//! Like the build processor: the controller and the subscription start on the
//! first registration, an event that cannot be decoded or arrives before then is
//! dropped (the next `AgentDeclared` repairs the canonical node), and every write
//! is an idempotent MERGE. The client or the controller ending ends the agent.

use crate::{AGENT_PROCESSOR_NAME, project};
use cassini_types::{ClientEvent, WireTraceCtx};
use polar::cassini::{CassiniClient, SubscribeRequest, TcpClient};
use polar::graph::controller::{
    GraphController, GraphControllerActor, GraphControllerArgs, GraphControllerMsg, GraphSignal,
};
use polar::topics::AGENT_LIFECYCLE_EVENTS;
use polar::{AgentLifecycleEvent, RkyvError};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

pub struct AgentProcessorSupervisor;

pub struct AgentProcessorState {
    client: Arc<dyn CassiniClient>,
    graph: Option<GraphController>,
}

pub enum AgentProcessorMessage {
    Client(ClientEvent),
    Graph(GraphSignal),
}

#[ractor::async_trait]
impl Actor for AgentProcessorSupervisor {
    type Msg = AgentProcessorMessage;
    type State = AgentProcessorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        // Fails startup fast when the graph's configuration is missing.
        polar::get_neo_config()?;
        let client = Arc::new(
            TcpClient::spawn(AGENT_PROCESSOR_NAME, myself.clone(), |event| {
                Some(AgentProcessorMessage::Client(event))
            })
            .await?,
        );
        Ok(AgentProcessorState {
            client,
            graph: None,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            AgentProcessorMessage::Client(ClientEvent::Registered { .. }) => {
                if state.graph.is_none() {
                    let signals = Arc::new(OutputPort::<GraphSignal>::default());
                    signals.subscribe(myself.clone(), |s| Some(AgentProcessorMessage::Graph(s)));
                    let (graph, _) = Actor::spawn_linked(
                        Some(format!("{AGENT_PROCESSOR_NAME}.graph.controller")),
                        GraphControllerActor,
                        GraphControllerArgs {
                            signal_port: signals,
                        },
                        myself.clone().into(),
                    )
                    .await?;
                    state.graph = Some(graph);
                }
                // Subscriptions belong to the session: made again on every registration.
                state.client.subscribe(SubscribeRequest {
                    topic: AGENT_LIFECYCLE_EVENTS.to_string(),
                    trace_ctx: WireTraceCtx::from_current_span(),
                })?;
                info!("subscribed to {AGENT_LIFECYCLE_EVENTS}");
            }
            AgentProcessorMessage::Client(ClientEvent::MessagePublished {
                topic, payload, ..
            }) => {
                let Some(graph) = &state.graph else {
                    warn!("graph controller not ready; dropping a message on {topic}");
                    return Ok(());
                };
                match decode(&payload) {
                    Some(event) => dispatch(graph, &event),
                    None => warn!("not an agent lifecycle event on {topic}; dropped"),
                }
            }
            AgentProcessorMessage::Client(ClientEvent::TransportError { reason }) => {
                warn!("Cassini transport error (awaiting reconnect): {reason}");
            }
            AgentProcessorMessage::Client(_) => {}
            AgentProcessorMessage::Graph(GraphSignal::Available) => debug!("graph available"),
            AgentProcessorMessage::Graph(GraphSignal::OpFailed(reason)) => {
                warn!("graph write failed: {reason}")
            }
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
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                let reason = format!("{:?} stopped: {reason:?}", cell.get_name());
                error!("{reason}");
                myself.stop(Some(reason));
            }
            SupervisionEvent::ActorFailed(cell, e) => {
                let reason = format!("{:?} failed: {e}", cell.get_name());
                error!("{reason}");
                myself.stop(Some(reason));
            }
            _ => {}
        }
        Ok(())
    }
}

/// rkyv from the operator; JSON accepted too, for a hand-sent event.
fn decode(payload: &[u8]) -> Option<AgentLifecycleEvent> {
    rkyv::from_bytes::<AgentLifecycleEvent, RkyvError>(payload)
        .ok()
        .or_else(|| serde_json::from_slice(payload).ok())
}

fn dispatch(graph: &GraphController, event: &AgentLifecycleEvent) {
    debug!(agent = %event.agent(), ?event, "projecting");
    for op in project(event) {
        if let Err(e) = graph.cast(GraphControllerMsg::Op(op)) {
            error!("cannot send a graph operation to the controller: {e}");
            return;
        }
    }
}
