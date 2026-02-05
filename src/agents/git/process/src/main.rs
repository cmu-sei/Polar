use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs};
use cassini_types::ClientEvent;
use git_agent_common::{GitRepositoryMessage, RepoId};
use neo4rs::{BoltType, Graph};
use polar::get_neo_config;
use polar::graph::{
    handle_op, rel, GraphControllerMsg, GraphControllerState, GraphNodeKey, GraphOp, GraphValue,
    Property,
};
use polar::SupervisorMessage;
use polar::GIT_REPOSITORIES_TOPIC;
use ractor::async_trait;
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent};
use std::sync::Arc;
use tracing::{debug, error, trace, warn};

pub const SERVICE_NAME: &str = "polar.git.procesor";

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
        name: String,
        email: String,
    },
}

impl GraphNodeKey for GitNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            GitNodeKey::Repository { repo_id } => (
                "(:GitRepository { id: $repo_id })".to_string(),
                vec![("repo_id".to_string(), repo_id.to_string().into())],
            ),

            GitNodeKey::Commit { oid } => (
                "(:GitCommit { oid: $oid })".to_string(),
                vec![("oid".to_string(), oid.clone().into())],
            ),

            GitNodeKey::Ref { repo_id, name } => (
                format!("(:GitRef {{ repo_id: ${prefix}_repo_id, name: ${prefix}_name }})"),
                vec![
                    (format!("{prefix}_repo_id"), repo_id.to_string().into()),
                    (format!("{prefix}_name"), name.clone().into()),
                ],
            ),

            GitNodeKey::Author { name, email } => (
                "(:GitAuthor { email: $email })".to_string(),
                vec![
                    ("email".to_string(), email.clone().into()),
                    // name should be SET, not matched on
                ],
            ),
        }
    }
}

// === Supervisor state ===
pub struct GitRepoProcessingManagerState {
    pub tcp_client: ActorRef<cassini_client::TcpClientMessage>,
    pub graph_controller: Option<ActorRef<GraphControllerMsg<GitNodeKey>>>,
}

// === Supervisor definition ===

pub struct GitRepoProcessingManager;

impl GitRepoProcessingManager {
    /// Generate canonical graph operations for a discovered commit.
    /// Does NOT touch refs; strictly immutable commit data and topology.
    fn ops_for_commit_discovered(ev: GitRepositoryMessage) -> Vec<GraphOp<GitNodeKey>> {
        let GitRepositoryMessage::CommitDiscovered {
            repo,
            oid,
            time,
            message,
            committer,
            parents,
            ..
        } = ev
        else {
            unreachable!(
                "ops_for_commit_discovered should only be called with CommitDiscovered events"
            );
        };

        let repo_key = GitNodeKey::Repository {
            repo_id: repo.clone(),
        };
        let commit_key = GitNodeKey::Commit { oid: oid.clone() };

        let mut ops = vec![
            // Ensure repo exists
            GraphOp::UpsertNode {
                key: repo_key.clone(),
                props: vec![],
            },
            // Ensure commit exists with metadata
            GraphOp::UpsertNode {
                key: commit_key.clone(),
                props: vec![
                    Property("message".into(), GraphValue::String(message)),
                    Property("authored_time".into(), GraphValue::I64(time)),
                    Property("committer".into(), GraphValue::String(committer)),
                ],
            },
            // Repository contains commit
            GraphOp::EnsureEdge {
                from: repo_key.clone(),
                to: commit_key.clone(),
                rel_type: rel::CONTAINS.into(),
                props: vec![],
            },
        ];

        // Parent edges (guard against self-referential edges)
        for parent_oid in parents {
            if parent_oid == oid {
                trace!("Skipping self-parent edge for commit {}", oid);
                continue;
            }

            ops.push(GraphOp::EnsureEdge {
                from: commit_key.clone(),
                to: GitNodeKey::Commit { oid: parent_oid },
                rel_type: "PARENT".into(),
                props: vec![],
            });
        }

        ops
    }

    /// Generate graph operations for a ref update.
    /// This is the *authoritative* source for ref → commit relationships.
    fn ops_for_ref_updated(ev: GitRepositoryMessage) -> Vec<GraphOp<GitNodeKey>> {
        let GitRepositoryMessage::RefUpdated {
            repo,
            ref_name,
            new,
            observed_at,
            ..
        } = ev
        else {
            unreachable!("ops_for_ref_updated should only be called with RefUpdated events");
        };

        let ref_key = GitNodeKey::Ref {
            repo_id: repo.clone(),
            name: ref_name.clone(),
        };
        let commit_key = GitNodeKey::Commit { oid: new.clone() };
        let repo_key = GitNodeKey::Repository {
            repo_id: repo.clone(),
        };

        vec![
            // Ensure repo exists
            GraphOp::UpsertNode {
                key: repo_key,
                props: vec![],
            },
            // Ensure ref exists
            GraphOp::UpsertNode {
                key: ref_key.clone(),
                props: vec![],
            },
            // Ensure commit exists (it may not have been observed yet)
            GraphOp::UpsertNode {
                key: commit_key.clone(),
                props: vec![],
            },
            // Connect ref to commit with timestamp
            GraphOp::EnsureEdge {
                from: ref_key,
                to: commit_key,
                rel_type: rel::POINTS_TO.into(),
                props: vec![Property(
                    "observed_at".into(),
                    GraphValue::String(observed_at),
                )],
            },
        ]
    }

    pub fn deserialize_and_dispatch(
        topic: String,
        payload: Vec<u8>,
        state: &GitRepoProcessingManagerState,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Received message on topic {topic}");

        let event = rkyv::from_bytes::<GitRepositoryMessage, rkyv::rancor::Error>(&payload)?;

        let ops = match event {
            GitRepositoryMessage::CommitDiscovered { .. } => Self::ops_for_commit_discovered(event),
            GitRepositoryMessage::RefUpdated { .. } => Self::ops_for_ref_updated(event),
            _ => {
                debug!("Skipping unhandled GitRepositoryMessage variant");
                vec![]
            }
        };

        for op in ops {
            if let Some(ctrl) = &state.graph_controller {
                ctrl.cast(GraphControllerMsg::Op(op))?;
            }
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
        let events_output = Arc::new(OutputPort::default());

        events_output.subscribe(myself.clone(), |event| {
            debug!("Received event: {event:?}");
            Some(SupervisorMessage::ClientEvent { event })
        });
        let client_config = TCPClientConfig::new()?;
        // start client
        let client_args = TcpClientArgs {
            config: client_config,
            registration_id: None,
            events_output: events_output.clone(),
        };

        let (tcp_client, _) = Actor::spawn_linked(
            Some("polar.git.processor.tcp".to_string()),
            cassini_client::TcpClientActor,
            client_args,
            myself.clone().into(),
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
                    //
                    debug!("Subscribing to topic {}", GIT_REPOSITORIES_TOPIC);
                    state
                        .tcp_client
                        .cast(cassini_client::TcpClientMessage::Subscribe(
                            GIT_REPOSITORIES_TOPIC.to_string(),
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

/// A concrete instance of a GraphController for the artifact linker.

pub struct GitRepoGraphController;

#[ractor::async_trait]
impl Actor for GitRepoGraphController {
    type Msg = GraphControllerMsg<GitNodeKey>;
    type State = GraphControllerState;
    type Arguments = Graph;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        graph: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting. Connecting to neo4j.");
        Ok(GraphControllerState { graph })
    }

    async fn handle(
        &self,
        _me: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            GraphControllerMsg::Op(op) => handle_op(&state.graph, &op).await?,
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    polar::init_logging(SERVICE_NAME.to_string());

    let (agent, handle) = Actor::spawn(
        Some(format!("{SERVICE_NAME}.supervisor")),
        GitRepoProcessingManager,
        (),
    )
    .await
    .unwrap();

    handle.await.unwrap();
}
