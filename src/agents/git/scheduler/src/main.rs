use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::{ClientEvent, WireTraceCtx};
use git_agent_common::{ConfigurationEvent, RepoId, RepoObservationConfig, GIT_REPO_CONFIG_EVENTS};
use neo4rs::{query, Graph};
use polar::{
    get_neo_config, GitRepositoryDiscoveredEvent, RkyvError, SupervisorMessage,
    GIT_REPO_DISCOGERY_TOPIC,
};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent};
use rkyv::{from_bytes, rancor, to_bytes};
use tracing::{debug, error, info, instrument, trace, warn};
pub const SERVICE_NAME: &str = "polar.git.scheduler";
pub const TCP: &str = "tcp";

pub struct RootSupervisor;

pub struct RootSupervisorState {
    tcp_client: ActorRef<TcpClientMessage>,
}

#[instrument(level = "trace", skip(graph))]
pub async fn fetch_repo_observation_config(
    graph: &Graph,
    repo_id: &RepoId,
) -> Result<Option<RepoObservationConfig>, ActorProcessingErr> {
    let cypher = r#"
        MATCH (c:RepoObservationConfig { repo_id: $repo_id })
        RETURN
            c.repo_id        AS repo_id,
            c.repo_url       AS repo_url,
            c.remotes        AS remotes,
            c.max_depth      AS max_depth,
            c.tracked_refs   AS tracked_refs
        LIMIT 1
    "#;

    let mut result = graph
        .execute(query(cypher).param("repo_id", repo_id.to_string()))
        .await?;

    if let Ok(Some(row)) = result.next().await {
        let repo_id: String = row.get("repo_id")?;
        let repo_url: String = row.get("repo_url")?;
        let remotes: Vec<String> = row.get("remotes")?;
        let max_depth: Option<i64> = row.get("max_depth")?;
        let tracked_refs: Vec<String> = row.get("tracked_refs")?;

        Ok(Some(RepoObservationConfig::new(
            RepoId::new(repo_id),
            repo_url,
            remotes,
            max_depth.map(|v| v as usize),
            tracked_refs,
        )))
    } else {
        Ok(None)
    }
}

impl RootSupervisor {
    #[instrument(skip_all, level = "debug")]
    async fn init(
        myself: ActorRef<SupervisorMessage>,
        state: &mut RootSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} initializing");
        // subscribe to supervision events
        state.tcp_client.cast(TcpClientMessage::Subscribe(
            GIT_REPO_DISCOGERY_TOPIC.to_string(),
        ))?;

        Ok(())
    }

    #[instrument(level = "trace", skip(payload, state))]
    pub async fn deserialize_and_dispatch(
        topic: String,
        payload: Vec<u8>,
        state: &mut RootSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        trace!("Received message from topic {topic}");
        let event = from_bytes::<GitRepositoryDiscoveredEvent, rancor::Error>(&payload)?;

        let graph = Graph::connect(get_neo_config()?)?;

        // Handle configuration request
        // Example: Fetch repository configuration and send response
        // query neo4j and send response
        // TODO: Implement fetching repository configuration from Neo4j, rem
        // TODO: Implement checking for an ssh or http url
        let repo_url = event.http_url.unwrap();
        let repo_id = RepoId::from_url(&repo_url);

        match fetch_repo_observation_config(&graph, &repo_id).await {
            Ok(Some(fetched_conig)) => {
                debug!("Fetched configuration for repo {}", repo_id.to_string());

                let payload = to_bytes::<RkyvError>(&fetched_conig)?;

                // Send response to the client
                let message = TcpClientMessage::Publish {
                    topic: GIT_REPO_CONFIG_EVENTS.to_string(),
                    payload: payload.to_vec(),
                    trace_ctx: WireTraceCtx::from_current_span(),
                };

                state.tcp_client.cast(message)?;
            }
            Ok(None) => {
                debug!(
                    "Couldn't find configuration for repo {}",
                    repo_id.to_string()
                );

                let response = ConfigurationEvent {
                    config: RepoObservationConfig::new(
                        repo_id,
                        repo_url,
                        vec!["origin".to_string()],
                        Some(100),
                        vec!["refs/heads/main".to_string()],
                    ),
                };
                debug!("Returning default configuration");
                let payload = to_bytes::<RkyvError>(&response)?;

                // Send response to the client
                let message = TcpClientMessage::Publish {
                    topic: GIT_REPO_CONFIG_EVENTS.to_string(),
                    payload: payload.to_vec(),
                    trace_ctx: WireTraceCtx::from_current_span(),
                };

                state.tcp_client.cast(message)?;
            }
            Err(e) => {
                error!("Error querying graph for configuration.");
                return Err(e);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Actor for RootSupervisor {
    type Msg = SupervisorMessage;
    type State = RootSupervisorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        // Read Kubernetes credentials and other data from the environment
        debug!("{myself:?} starting");

        let events_output = std::sync::Arc::new(OutputPort::default());

        //subscribe to registration event
        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        let config = TCPClientConfig::new()?;

        let (tcp_client, _) = Actor::spawn_linked(
            Some(format!("{SERVICE_NAME}.{TCP}")),
            TcpClientActor,
            TcpClientArgs {
                config,
                registration_id: None,
                events_output: Some(events_output),
                event_handler: None,
            },
            myself.clone().into(),
        )
        .await?;
        Ok(RootSupervisorState { tcp_client })
    }

    async fn handle_supervisor_evt(
        &self,
        _: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                info!(
                    "CLUSTER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!(
                    "CLUSTER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }

        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::ControlResponse {
                    registration_id,
                    result,
                    trace_ctx,
                } => todo!("handle control response"),
                ClientEvent::Registered { .. } => Self::init(myself, state).await?,
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    Self::deserialize_and_dispatch(topic, payload, state).await?
                }
                ClientEvent::TransportError { reason } => {
                    error!("Transport error occurred! {reason}");
                    myself.stop(Some(reason))
                }
            },
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    polar::init_logging(SERVICE_NAME.to_string());

    let (scheduler, handle) = Actor::spawn(
        Some(format!("{SERVICE_NAME}.supervisor")),
        RootSupervisor,
        (),
    )
    .await
    .unwrap();

    handle.await.unwrap();
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use neo4rs::query;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::neo4j::Neo4j;

    #[tokio::test]
    async fn fetch_repo_config_returns_config_when_present() {
        let container = Neo4j::default().start().await.unwrap();

        // prepare neo4rs client
        let config = neo4rs::ConfigBuilder::new()
            .uri(format!(
                "bolt://{}:{}",
                container.get_host().await.unwrap(),
                container.image().bolt_port_ipv4().unwrap()
            ))
            .user(container.image().user().expect("default user is set"))
            .password(
                container
                    .image()
                    .password()
                    .expect("default password is set"),
            )
            .build()
            .unwrap();

        // connect ot Neo4j
        let graph = neo4rs::Graph::connect(config).unwrap();
        // Prepare test RepoObservationConfig
        let repo_id = RepoId::new("test-repo-123".into());
        let repo_url = "https://github.com/test/repo.git";
        let remotes = vec!["origin".to_string()];
        let max_depth = Some(100);
        let tracked_refs = vec!["refs/heads/main".to_string()];

        // Clean previous data just in case
        graph
            .run(
                query("MATCH (c:RepoObservationConfig { repo_id: $id }) DELETE c")
                    .param("id", repo_id.to_string()),
            )
            .await
            .unwrap();

        // Insert test config
        graph
            .run(
                query(
                    r#"
                    CREATE (c:RepoObservationConfig {
                        repo_id: $repo_id,
                        repo_url: $repo_url,
                        remotes: $remotes,
                        max_depth: $max_depth,
                        tracked_refs: $tracked_refs
                    })
                "#,
                )
                .param("repo_id", repo_id.to_string())
                .param("repo_url", repo_url)
                .param("remotes", remotes.clone())
                .param("max_depth", max_depth.unwrap() as i64)
                .param("tracked_refs", tracked_refs.clone()),
            )
            .await
            .unwrap();

        // Fetch via our function
        let fetched = fetch_repo_observation_config(&graph, &repo_id)
            .await
            .unwrap()
            .expect("config should exist");

        assert_eq!(fetched.repo_id.to_string(), repo_id.to_string());
        assert_eq!(fetched.repo_url, repo_url);
        assert_eq!(fetched.remotes, remotes);
        assert_eq!(fetched.max_depth, max_depth.unwrap());
    }

    #[tokio::test]
    async fn fetch_repo_config_returns_none_when_missing() {
        let container = Neo4j::default().start().await.unwrap();

        // prepare neo4rs client
        let config = neo4rs::ConfigBuilder::new()
            .uri(format!(
                "bolt://{}:{}",
                container.get_host().await.unwrap(),
                container.image().bolt_port_ipv4().unwrap()
            ))
            .user(container.image().user().expect("default user is set"))
            .password(
                container
                    .image()
                    .password()
                    .expect("default password is set"),
            )
            .build()
            .unwrap();

        // connect ot Neo4j
        let graph = neo4rs::Graph::connect(config).unwrap();

        let repo_id = RepoId::new("non-existent-repo".into());

        let fetched = fetch_repo_observation_config(&graph, &repo_id)
            .await
            .unwrap();

        assert!(fetched.is_none());
    }
}
