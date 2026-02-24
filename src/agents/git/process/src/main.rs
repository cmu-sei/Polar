use cassini_client::TcpClient;
use cassini_types::ClientEvent;
use chrono::Utc;
use git_agent_common::{GitRepositoryMessage, RepoId, GIT_REPO_PROCESSING_TOPIC};
use neo4rs::BoltType;
use polar::get_neo_config;
use polar::graph::{
    rel, GraphController, GraphControllerMsg, GraphNodeKey, GraphOp, GraphValue, Property,
};
use polar::{impl_graph_controller, SupervisorMessage};
use ractor::async_trait;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use rkyv::rancor;
use tracing::{debug, error, trace, warn};

#[derive(Debug, Clone)]
pub enum GitNodeKey {
    Repository {
        repo_id: RepoId, // your canonical UUID / v5
    },

    Commit {
        oid: String, // full hex
    },

    Ref {
        repo_id: RepoId,
        name: String, // refs/heads/main, refs/tags/v1.2.3
    },

    Author {
        email: String,
    },
}

impl GraphNodeKey for GitNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            GitNodeKey::Repository { repo_id } => (
                format!("(:GitRepository {{ id: ${prefix}_repo_id }})"),
                vec![(format!("{prefix}_repo_id"), repo_id.to_string().into())],
            ),

            GitNodeKey::Commit { oid } => (
                format!("(:GitCommit {{ oid: ${prefix}_oid }})"),
                vec![(format!("{prefix}_oid"), oid.clone().into())],
            ),

            GitNodeKey::Ref { repo_id, name } => (
                format!("(:GitRef {{ repo_id: ${prefix}_repo_id, name: ${prefix}_name }})"),
                vec![
                    (format!("{prefix}_repo_id"), repo_id.to_string().into()),
                    (format!("{prefix}_name"), name.clone().into()),
                ],
            ),
            GitNodeKey::Author { email } => (
                format!("(:GitAuthor {{ email: ${prefix}_email }})"),
                vec![
                    (format!("{prefix}_email"), email.clone().into()),
                    // name should be SET, not matched on
                ],
            ),
        }
    }
}

// === Supervisor state ===
pub struct GitRepoProcessingManagerState {
    pub tcp_client: TcpClient,
    pub graph_controller: Option<ActorRef<GraphControllerMsg<GitNodeKey>>>,
}

// === Supervisor definition ===

pub struct GitRepoProcessingManager;

impl GitRepoProcessingManager {
    /// Generate canonical graph operations for a discovered commit.
    /// Does NOT touch refs; strictly immutable commit data and topology.
    fn ops_for_commit_discovered(
        graph_controller: &GraphController<GitNodeKey>,
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

                // Ensure repo exists
                graph_controller.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: repo_key.clone(),
                    props: vec![],
                }))?;
                // Ensure commit exists with metadata
                graph_controller.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: commit_key.clone(),
                    props: vec![
                        Property("message".into(), GraphValue::String(message)),
                        Property("authored_time".into(), GraphValue::I64(time)),
                        Property("committer".into(), GraphValue::String(committer)),
                        Property(
                            "observed_at".into(),
                            GraphValue::String(Utc::now().to_rfc3339()),
                        ),
                    ],
                }))?;
                // Repository contains commit
                graph_controller.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                    from: repo_key.clone(),
                    to: commit_key.clone(),
                    rel_type: rel::CONTAINS.into(),
                    props: vec![],
                }))?;

                // Parent edges (guard against self-referential edges)
                for parent_oid in parents {
                    if parent_oid == oid {
                        trace!("Skipping self-parent edge for commit {}", oid);
                        continue;
                    }

                    graph_controller.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                        from: commit_key.clone(),
                        to: GitNodeKey::Commit { oid: parent_oid },
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
        graph_controller: &GraphController<GitNodeKey>,
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

                // Ensure repo exists
                graph_controller.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: repo_key,
                    props: vec![],
                }))?;

                // Ensure ref exists
                graph_controller.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: ref_key.clone(),
                    props: vec![],
                }))?;

                // Ensure commit exists (it may not have been observed yet)
                graph_controller.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: commit_key.clone(),
                    props: vec![],
                }))?;

                // Connect ref to commit with timestamp
                graph_controller.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                    from: ref_key,
                    to: commit_key,
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
        let tcp_client = polar::spawn_tcp_client(
            &format!("{GIT_REPO_PROCESSING_TOPIC}.tcp"),
            myself.into(),
            |ev| Some(SupervisorMessage::ClientEvent { event: ev }),
        )
        .await?;

        let s = GitRepoProcessingManagerState {
            tcp_client,
            graph_controller: None,
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
                    // get graph connection

                    let graph = neo4rs::Graph::connect(get_neo_config()?)?;

                    let (controller, _) = Actor::spawn_linked(
                        Some("linker.graph.controller".to_string()),
                        crate::GitRepoGraphController,
                        graph,
                        myself.clone().into(),
                    )
                    .await?;

                    // subscribe to topic

                    debug!("Subscribing to topic {}", GIT_REPO_PROCESSING_TOPIC);
                    state
                        .tcp_client
                        .cast(cassini_client::TcpClientMessage::Subscribe(
                            GIT_REPO_PROCESSING_TOPIC.to_string(),
                        ))?;

                    state.graph_controller = Some(controller);
                }
                ClientEvent::MessagePublished { topic, payload } => {
                    Self::deserialize_and_dispatch(topic, payload, state)?
                }
                ClientEvent::TransportError { reason } => {
                    error!("Transport error: {reason}");
                    myself.stop(Some(reason))
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
                todo!("Implement some restart logic");
            }
            SupervisionEvent::ActorTerminated(name, _state, reason) => {
                warn!("Actor {name:?} stopped! {reason:?}");
            }
            SupervisionEvent::ActorStarted(actor) => {
                debug!("Actor {actor:?} started!");
            }
            _ => {}
        }
        Ok(())
    }
}

// A concrete instance of a GraphController for the artifact linker.
impl_graph_controller!(GitRepoGraphController, node_key = GitNodeKey);

#[tokio::main]
async fn main() {
    polar::init_logging(GIT_REPO_PROCESSING_TOPIC.to_string());

    let (_agent, handle) = Actor::spawn(
        Some(format!("{GIT_REPO_PROCESSING_TOPIC}.supervisor")),
        GitRepoProcessingManager,
        (),
    )
    .await
    .unwrap();

    handle.await.unwrap();
}
