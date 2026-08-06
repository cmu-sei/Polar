use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::{ClientEvent, WireTraceCtx};
use git_agent_common::{ConfigurationEvent, GIT_REPO_CONFIG_EVENTS, RepoObservationConfig};
use neo4rs::{Graph, query};
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use polar::{
    GIT_REPO_DISCOGERY_TOPIC, GitRepositoryDiscoveredEvent, RkyvError, SupervisorMessage,
    UNEXPECTED_MESSAGE_STR, get_neo_config, graph::nodes::git::RepoId,
};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};
use rkyv::{from_bytes, rancor, to_bytes};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, trace, warn};

pub const SERVICE_NAME: &str = "polar.git.scheduler";
pub const TCP: &str = "tcp";
const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

pub struct RootSupervisor;

pub struct RootSupervisorState {
    tcp_client: ActorRef<TcpClientMessage>,
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
}

// fetch_repo_observation_config unchanged, omitted here for brevity

impl RootSupervisor {
    #[instrument(skip_all, level = "debug")]
    async fn init(
        myself: ActorRef<SupervisorMessage>,
        state: &mut RootSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} initializing");
        state.tcp_client.cast(TcpClientMessage::Subscribe {
            topic: GIT_REPO_DISCOGERY_TOPIC.to_string(),
            trace_ctx: None,
        })?;
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

        let repo_url = event.http_url.unwrap();
        let repo_id = RepoId::from_url(&repo_url);

        let config = match fetch_repo_observation_config(&graph, &repo_id).await {
            Ok(Some(config)) => {
                debug!("Fetched stored configuration for repo {}", repo_id);
                config
            }
            Ok(None) => {
                debug!(
                    "No stored configuration for repo {} — using defaults",
                    repo_id
                );
                RepoObservationConfig::new(
                    repo_id,
                    repo_url,
                    vec!["origin".to_string()],
                    Some(100),
                    vec!["refs/heads/main".to_string()],
                )
            }
            Err(e) => {
                error!("Error querying graph for repo configuration");
                return Err(e);
            }
        };

        let event = ConfigurationEvent { config };
        let payload = to_bytes::<RkyvError>(&event)?;

        state.tcp_client.cast(TcpClientMessage::Publish {
            topic: GIT_REPO_CONFIG_EVENTS.to_string(),
            payload: payload.to_vec(),
            trace_ctx: WireTraceCtx::from_current_span(),
        })?;

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
        debug!("{myself:?} starting");

        // --- NEW: healthcheck wiring ---
        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        // No persistent GraphController here -- each dispatch does its own
        // ad hoc Graph::connect(). No signal path exists to track real
        // graph availability (same limitation as jira-processor's
        // children), so expects_graph is false; cert rejuvenation still
        // applies, and the Neo4j dep cert is still checked.
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
                expects_graph: false,
                rejuvenation_threshold_secs: 300,
                dep_cert_endpoints,
                prepare_shutdown_port,
            },
            myself.get_cell(),
        )
        .await
        .map_err(|e| ActorProcessingErr::from(e))?;
        // --- end NEW ---

        let events_output = std::sync::Arc::new(OutputPort::default());
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

        Ok(RootSupervisorState {
            tcp_client,
            healthcheck,
            draining: false,
        })
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                let actor_name = actor_cell.get_name().unwrap_or_default();
                info!(
                    "CLUSTER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_name,
                    actor_cell.get_id()
                );
                // --- NEW ---
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
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!(
                    "CLUSTER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                // BUG FIX: previously logged only, no exit trigger at all.
                if !state.draining {
                    state.draining = true;
                    warn!("entering drain window before exit");
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
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
            SupervisorMessage::Heartbeat => {}
            // --- NEW ---
            SupervisorMessage::PrepareShutdown => {
                info!("PrepareShutdown received");
                if let Err(e) = state.healthcheck.cast(HealthCheckMessage::ShutdownAck) {
                    error!("failed to send ShutdownAck: {e}");
                }
            }
            SupervisorMessage::GraphSignal(_) => {}
            SupervisorMessage::ForceExit => {
                warn!("drain window elapsed, exiting now");
                std::process::exit(1);
            }
            // --- end NEW ---
            SupervisorMessage::ClientEvent { event } => match event {
                // BUG FIX: was `todo!("handle control response")` -- a live
                // panic waiting to happen on any control response.
                ClientEvent::ControlResponse { .. } => {
                    warn!("ControlResponse received but not handled (non-fatal)");
                }
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);
                    Self::init(myself, state).await?
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
                        Self::deserialize_and_dispatch(topic, payload, state).await?
                    }
                    // --- end NEW ---
                }
                // BUG FIX: was `myself.stop(Some(reason))` -- silent exit-0,
                // Job marks Completed, no restart, no visible failure.
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniDisconnected);
                }
                _ => warn!("{UNEXPECTED_MESSAGE_STR}"),
            },
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    polar::init_logging(SERVICE_NAME.to_string());

    let (_scheduler, handle) = Actor::spawn(
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

        let graph = neo4rs::Graph::connect(config).unwrap();

        let repo_id = RepoId::new("test-repo-123".into());
        let repo_url = "https://github.com/test/repo.git";
        let remotes = vec!["origin".to_string()];
        let max_depth = Some(100);
        let tracked_refs = vec!["refs/heads/main".to_string()];

        graph
            .run(
                query("MATCH (c:RepoObservationConfig { repo_id: $id }) DELETE c")
                    .param("id", repo_id.to_string()),
            )
            .await
            .unwrap();

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

        let fetched = fetch_repo_observation_config(&graph, &repo_id)
            .await
            .unwrap()
            .expect("config should exist");

        assert_eq!(fetched.repo_id.to_string(), repo_id.to_string());
        assert_eq!(fetched.repo_url, repo_url);
        assert_eq!(fetched.remotes, remotes);
        assert_eq!(fetched.max_depth, max_depth.unwrap());
        // credentials should always be None until CredentialAgent is wired in
        assert!(fetched.credentials.is_none());
    }

    #[tokio::test]
    async fn fetch_repo_config_returns_none_when_missing() {
        let container = Neo4j::default().start().await.unwrap();

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

        let graph = neo4rs::Graph::connect(config).unwrap();
        let repo_id = RepoId::new("non-existent-repo".into());
        let fetched = fetch_repo_observation_config(&graph, &repo_id)
            .await
            .unwrap();
        assert!(fetched.is_none());
    }
}

/// Fetch a stored `RepoObservationConfig` from the graph for a known repo.
///
/// Returns `None` if no config node exists for the given `repo_id`, in which
/// case the caller should produce a default config.
///
/// # Credentials
///
/// The graph-stored config does not yet include credentials. Credentials will
/// be sourced from the CredentialAgent at dispatch time once that agent is
/// implemented. For now, `credentials` is always `None` — callers must treat
/// the returned config as suitable for public repos only, or augment it with
/// credentials from another source before dispatching.
///
/// TODO: fetch credentials from CredentialAgent and populate
///       `RepoObservationConfig::credentials` before publishing the
///       `ConfigurationEvent`.
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
