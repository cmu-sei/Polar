use crate::{
    GitAgentConfig, GitRepoSupervisor, GitRepoSupervisorArgs, REPO_SUPERVISOR_NAME,
    RepoSupervisorMessage, SERVICE_NAME,
};
use git_agent_common::{ConfigurationEvent, GIT_REPO_CONFIG_EVENTS};
use cassini_types::ClientEvent;
use polar::SupervisorMessage;
use polar::cassini::TcpClient;
use polar::cassini::CassiniClient;
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};
use rkyv::from_bytes;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, trace, warn};

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

pub struct RootSupervisor;

pub struct RootSupervisorState {
    tcp_client: TcpClient,
    repo_supervisor: Option<ActorRef<RepoSupervisorMessage>>,
    git_agent_config: GitAgentConfig,
    cache_root: std::path::PathBuf,
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
}

impl RootSupervisor {
    pub fn deserialize_and_dispatch(
        topic: String,
        payload: Vec<u8>,
        state: &mut RootSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Received message from topic {topic}");
        if let Ok(ev) = from_bytes::<ConfigurationEvent, rkyv::rancor::Error>(&payload) {
            if let Some(s) = &state.repo_supervisor {
                Ok(s.cast(RepoSupervisorMessage::SpawnWorker { config: ev.config })?)
            } else {
                Err("Failed to find repo supervisor".into())
            }
        } else {
            warn!("Failed to deserialize event");
            Ok(())
        }
    }

    #[instrument(skip_all, level = "debug")]
    async fn init(
        myself: ActorRef<SupervisorMessage>,
        state: &mut RootSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        let (repo_supervisor, _) = Actor::spawn_linked(
            Some(REPO_SUPERVISOR_NAME.to_string()),
            GitRepoSupervisor,
            GitRepoSupervisorArgs {
                tcp_client: state.tcp_client.clone(),
                cache_root: state.cache_root.clone(),
            },
            myself.into(),
        )
        .await?;

        state.repo_supervisor = Some(repo_supervisor);

        state.tcp_client.subscribe(polar::cassini::SubscribeRequest {
            topic: GIT_REPO_CONFIG_EVENTS.to_string(),
            trace_ctx: None,
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

        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        let dep_cert_endpoints = vec![
            DepCertEndpoint::parse("cassini-ip-svc.polar.svc.cluster.local:8080:300")
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

        // Startup validation: fail fast if the credential config is
        // missing/malformed. NOTE: not currently threaded into
        // deserialize_and_dispatch's RepoObservationConfig forwarding --
        // worth confirming with whoever wrote GitAgentConfig whether
        // per-repo credential injection happens elsewhere (inside
        // GitRepoSupervisor or its workers) or is still a TODO.
        let config_path =
            std::env::var("POLAR_GIT_AGENT_CONFIG").unwrap_or_else(|_| "git.yaml".to_string());
        let git_agent_config = GitAgentConfig::load(std::path::Path::new(&config_path))
            .map_err(|e| ActorProcessingErr::from(format!("failed to load {config_path}: {e}")))?;

        let cache_root = match std::env::var("POLAR_CACHE_ROOT") {
            Ok(path) => {
                debug!("Using cache root at {}", path);
                std::path::PathBuf::from(path)
            }
            Err(_) => {
                let default_dir = ".polar/cache";
                debug!(
                    "No POLAR_CACHE_ROOT set, using default cache directory {}",
                    default_dir
                );
                std::env::current_dir()
                    .map(|d| d.join(default_dir))
                    .map_err(|_| ActorProcessingErr::from("Failed to determine cache root"))?
            }
        };

        let tcp_client = TcpClient::spawn(SERVICE_NAME, myself, |event| {
            Some(SupervisorMessage::ClientEvent { event })
        })
        .await?;

        Ok(RootSupervisorState {
            tcp_client,
            repo_supervisor: None,
            git_agent_config,
            cache_root,
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
                debug!(
                    "{0:?}:{1:?} terminated. {reason:?}",
                    actor_name,
                    actor_cell.get_id()
                );
                if actor_name == HEALTHCHECK_ACTOR_NAME && !state.draining {
                    state.draining = true;
                    info!("entering drain window before exiting for rejuvenation");
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                error!(
                    "{0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
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
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);
                    if let Err(e) = Self::init(myself.clone(), state).await {
                        myself.stop(Some(e.to_string()));
                    }
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    if state.draining {
                        warn!(
                            "draining -- logging message on topic '{topic}' \
                             ({} bytes) instead of dispatching; will be lost on exit",
                            payload.len()
                        );
                    } else {
                        Self::deserialize_and_dispatch(topic, payload, state)?
                    }
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniDisconnected);
                }
                ClientEvent::PublishAcknowledged { .. } => trace!("{event:?}"),
                _ => (),
            },
        }
        Ok(())
    }
}
