use crate::{
    GitRepoSupervisor, GitRepoSupervisorArgs, REPO_SUPERVISOR_NAME, RepoSupervisorMessage,
    SERVICE_NAME,
};
use cassini_client::TcpClientMessage;
use cassini_types::ClientEvent;
use git_agent_common::{ConfigurationEvent, GIT_REPO_CONFIG_EVENTS};
use polar::SupervisorMessage;
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};
use rkyv::from_bytes;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

pub struct RootSupervisor;

pub struct RootSupervisorState {
    repo_supervisor: Option<ActorRef<RepoSupervisorMessage>>,
    tcp_client: ActorRef<TcpClientMessage>,
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
        let cache_root = match std::env::var("POLAR_CACHE_ROOT") {
            Ok(path) => {
                debug!("Using cache root at {}", path);
                PathBuf::from(path)
            }
            Err(_) => {
                let default_dir = ".polar/cache";
                debug!(
                    "No POLAR_CACHE_ROOT set, using default cache directory {}",
                    default_dir
                );
                if let Ok(current_dir) = std::env::current_dir() {
                    current_dir.join(default_dir)
                } else {
                    return Err(ActorProcessingErr::from("Failed to determine cache root"));
                }
            }
        };

        let (repo_supervisor, _) = Actor::spawn_linked(
            Some(REPO_SUPERVISOR_NAME.to_string()),
            GitRepoSupervisor,
            GitRepoSupervisorArgs {
                tcp_client: state.tcp_client.clone(),
                cache_root,
            },
            myself.into(),
        )
        .await?;

        state.repo_supervisor = Some(repo_supervisor);

        state.tcp_client.cast(TcpClientMessage::Subscribe {
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

        // --- NEW: healthcheck wiring ---
        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        // Observer -- Cassini-only, no graph dependency.
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
        // --- end NEW ---

        let tcp_client = polar::spawn_tcp_client(SERVICE_NAME, myself, |event| {
            Some(SupervisorMessage::ClientEvent { event })
        })
        .await?;

        Ok(RootSupervisorState {
            tcp_client,
            repo_supervisor: None,
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

                // --- NEW: exit on clean healthcheck termination (rejuvenation) ---
                if actor_name == HEALTHCHECK_ACTOR_NAME && !state.draining {
                    state.draining = true;
                    info!(
                        "entering drain window before exiting for rejuvenation"
                    );
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
                // --- end NEW ---
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                error!(
                    "{0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );

                // BUG FIX: previously this only logged an error with no
                // further action -- the pod would keep running indefinitely
                // in a broken state after any child actor failure (e.g.
                // repo_supervisor), with nothing to trigger a Job restart.
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
            // --- NEW: real PrepareShutdown handling (was a no-op) ---
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
                ClientEvent::Registered { .. } => {
                    // --- NEW ---
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);
                    // --- end NEW ---
                    if let Err(e) = Self::init(myself.clone(), state).await {
                        myself.stop(Some(e.to_string()));
                    }
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
                // BUG FIX: was `myself.stop(Some(reason))`, which stops the
                // actor cleanly -- main() then returns normally and the
                // process exits 0. Under a Job with completions=1, an
                // exit-0 pod is marked Completed and never retried, so a
                // transient transport blip would silently and permanently
                // end this agent with no restart and no visible failure.
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniDisconnected);
                }
                ClientEvent::ControlResponse { .. } => {
                    error!("ControlResponse not implemented!");
                }
                _ => warn!("UNEXPECTED_MESSAGE_STR"),
            },
        }
        Ok(())
    }
}
