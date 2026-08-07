use cassini_types::ClientEvent;
use chrono::Utc;
use git_agent_common::GitRepositoryMessage;
use polar::SupervisorMessage;
use polar::cassini::{CassiniClient, SubscribeRequest, TcpClient};
use polar::graph::controller::GraphControllerActor;
use polar::graph::controller::IntoGraphKey;
use polar::graph::{
    controller::{GraphController, GraphControllerMsg, GraphOp, GraphSignal, GraphValue, Property, rel},
    nodes::git::GitNodeKey,
};
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use polar::topics::GIT_REPOSITORY_EVENTS;
use ractor::async_trait;
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent};
use rkyv::rancor;
use std::sync::Arc;
use tracing::{debug, error, info, trace, warn};

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

const SERVICE_NAME: &str = "git.repositories.processor";

pub struct GitRepoProcessingManagerState {
    pub tcp_client: TcpClient,
    pub graph_controller: Option<ActorRef<GraphControllerMsg>>,
    pub healthcheck: ActorRef<HealthCheckMessage>,
    pub draining: bool,
}

// === Supervisor definition ===

pub struct GitRepoProcessingManager;

impl GitRepoProcessingManager {
    /// Generate canonical graph operations for a discovered commit.
    /// Does NOT touch refs; strictly immutable commit data and topology.
    fn ops_for_commit_discovered(
        graph_controller: &GraphController,
        ev: GitRepositoryMessage,
    ) -> Result<(), ActorProcessingErr> {
        match ev {
            GitRepositoryMessage::CommitDiscovered {
                repo,
                oid,
                time,
                message,
                committer,
                parents,
                ..
            } => {
                let repo_key = GitNodeKey::Repository {
                    repo_id: repo.clone(),
                };
                let commit_key = GitNodeKey::Commit { oid: oid.clone() };

                graph_controller.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: repo_key.clone().into_key(),
                    props: vec![],
                }))?;
                graph_controller.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: commit_key.clone().into_key(),
                    props: vec![
                        Property("message".into(), GraphValue::String(message)),
                        Property("authored_time".into(), GraphValue::I64(time)),
                        Property("committer".into(), GraphValue::String(committer)),
                        Property(
                            "observed_at".into(),
                            GraphValue::I64(Utc::now().timestamp_millis()),
                        ),
                    ],
                }))?;
                graph_controller.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                    from: repo_key.clone().into_key(),
                    to: commit_key.clone().into_key(),
                    rel_type: rel::CONTAINS.into(),
                    props: vec![],
                }))?;

                for parent_oid in parents {
                    if parent_oid == oid {
                        trace!("Skipping self-parent edge for commit {}", oid);
                        continue;
                    }

                    graph_controller.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                        from: commit_key.clone().into_key(),
                        to: GitNodeKey::Commit { oid: parent_oid }.into_key(),
                        rel_type: "PARENT".into(),
                        props: vec![],
                    }))?;
                }
                Ok(())
            }
            _ => {
                warn!("Received unexpected event {ev:?}");
                Ok(())
            }
        }
    }

    /// Generate graph operations for a ref update.
    /// This is the *authoritative* source for ref → commit relationships.
    fn ops_for_ref_updated(
        graph_controller: &GraphController,
        ev: GitRepositoryMessage,
    ) -> Result<(), ActorProcessingErr> {
        match ev {
            GitRepositoryMessage::RefUpdated {
                repo,
                ref_name,
                new,
                observed_at,
                ..
            } => {
                let ref_key = GitNodeKey::Ref {
                    repo_id: repo.clone(),
                    name: ref_name.clone(),
                };
                let commit_key = GitNodeKey::Commit { oid: new.clone() };
                let repo_key = GitNodeKey::Repository {
                    repo_id: repo.clone(),
                };

                graph_controller.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: repo_key.into_key(),
                    props: vec![],
                }))?;

                graph_controller.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: ref_key.clone().into_key(),
                    props: vec![],
                }))?;

                graph_controller.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: commit_key.clone().into_key(),
                    props: vec![],
                }))?;

                graph_controller.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                    from: ref_key.into_key(),
                    to: commit_key.into_key(),
                    rel_type: rel::POINTS_TO.into(),
                    props: vec![Property(
                        "observed_at".into(),
                        GraphValue::String(observed_at),
                    )],
                }))?;

                Ok(())
            }
            _ => {
                warn!("Received unexpected event {ev:?}");
                Ok(())
            }
        }
    }

    pub fn deserialize_and_dispatch(
        topic: String,
        payload: Vec<u8>,
        state: &GitRepoProcessingManagerState,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Received message on topic {topic}");

        match rkyv::from_bytes::<GitRepositoryMessage, rancor::Error>(&payload) {
            Ok(msg) => {
                if let Some(ctrl) = &state.graph_controller {
                    match msg {
                        GitRepositoryMessage::CommitDiscovered { .. } => {
                            Self::ops_for_commit_discovered(ctrl, msg)?
                        }
                        GitRepositoryMessage::RefUpdated { .. } => {
                            Self::ops_for_ref_updated(ctrl, msg)?
                        }
                    }
                }
            }
            Err(_) => warn!("Failed to deserialize message"),
        }
        Ok(())
    }
}

#[async_trait]
impl Actor for GitRepoProcessingManager {
    type Msg = SupervisorMessage;
    type State = GitRepoProcessingManagerState;
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
        // This supervisor owns a GraphControllerActor directly (unlike
        // jira-processor, where children manage their own graph
        // connections), so real GraphAvailable/GraphOpFailed tracking
        // applies here.
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
        let tcp_client = TcpClient::spawn(&format!("{SERVICE_NAME}.tcp"), myself, |ev| {
            Some(SupervisorMessage::ClientEvent { event: ev })
        })
        .await?;
        let s = GitRepoProcessingManagerState {
            tcp_client,
            graph_controller: None,
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
                    let graph_signal_port = Arc::new(OutputPort::<GraphSignal>::default());
                    graph_signal_port.subscribe(myself.clone(), |signal| {
                        Some(SupervisorMessage::GraphSignal(signal))
                    });
                    // --- end NEW ---
                    let (controller, _) = Actor::spawn_linked(
                        Some("git.process.graph.controller".to_string()), // BUG FIX: was "linker.graph.controller" -- copy-pasted from provenance/linker, wrong actor name for this agent
                        GraphControllerActor,
                        polar::graph::controller::GraphControllerArgs { signal_port: graph_signal_port },
                        myself.get_cell(),
                    )
                    .await?;
                    // subscribe to topic
                    state.tcp_client.subscribe(SubscribeRequest {
                        topic: GIT_REPOSITORY_EVENTS.to_string(),
                        trace_ctx: None,
                    })?;
                    state.graph_controller = Some(controller);
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
                        Self::deserialize_and_dispatch(topic, payload, state)?
                    }
                    // --- end NEW ---
                }
                // BUG FIX: was `myself.stop(Some(reason))` -- silent exit-0,
                // Job marks Completed, no restart, no visible failure.
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniDisconnected);
                }
                ClientEvent::ControlResponse { .. } => {
                    error!("ControlResponse not implemented here!");
                }
                _ => warn!("UNEXPECTED_MESSAGE_STR"),
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
            // BUG FIX: was `todo!("Implement some restart logic")` -- a live
            // panic on any child actor failure (including the graph
            // controller itself). Now routes through the same bounded
            // drain-and-exit path as every other agent, so the Job
            // controller can actually restart with fresh init/certs instead
            // of the process panicking uncontrolled.
            SupervisionEvent::ActorFailed(actor_cell, err) => {
                error!("Actor {actor_cell:?} failed! {err:?}");
                let actor_name = actor_cell.get_name().unwrap_or_default();
                if !state.draining {
                    state.draining = true;
                    warn!("entering drain window before exit ({actor_name} failed)");
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            SupervisionEvent::ActorTerminated(actor_cell, _state, reason) => {
                warn!("Actor {actor_cell:?} stopped! {reason:?}");
                let actor_name = actor_cell.get_name().unwrap_or_default();
                // --- NEW: exit on clean healthcheck termination ---
                if actor_name == HEALTHCHECK_ACTOR_NAME && !state.draining {
                    state.draining = true;
                    info!("entering drain window before exiting for rejuvenation");
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

#[tokio::main]
async fn main() {
    polar::init_logging(SERVICE_NAME.to_string());

    let (_agent, handle) = Actor::spawn(
        Some(format!("{SERVICE_NAME}.supervisor")),
        GitRepoProcessingManager,
        (),
    )
    .await
    .unwrap();

    handle.await.unwrap();
}
