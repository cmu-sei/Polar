mod config;
mod git_ops;
mod sync_actor;
mod watcher;

use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent;
use polar::SupervisorMessage;
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use config::ObserverConfig;
use git_ops::ensure_repo;
use sync_actor::GitSyncActor;
use watcher::{GitWatcherActor, GitWatcherMsg};

const SERVICE_NAME: &str = "polar-scheduler-observer";
const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

// --- NEW: this agent previously had no unified root supervisor at all --
// tcp_client, watcher, and a RegistrationWaiter were spawned as
// independent, unlinked top-level actors in main(). None of them
// participated in the shared SupervisorMessage/healthcheck machinery every
// other agent uses, and none of their failures (other than the watcher's,
// via watcher_handle.await? in main) were tracked or acted on. There was
// also no cert rejuvenation whatsoever for this agent.
//
// RootSupervisor replaces that flat structure: it owns the TCP client
// connection, links the watcher under it, spawns HealthCheckActor for
// cert rejuvenation (Cassini-only -- this agent doesn't touch the graph),
// and reacts to ClientEvent::Registered by spawning the watcher and
// triggering its initial scan directly, eliminating the separate
// RegistrationWaiter actor entirely.
pub struct RootSupervisor;

pub struct RootSupervisorState {
    repo_path: String,
    tcp_client: ActorRef<TcpClientMessage>,
    watcher: Option<ActorRef<GitWatcherMsg>>,
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
}

pub struct RootSupervisorArgs {
    repo_path: String,
}

#[async_trait]
impl Actor for RootSupervisor {
    type Msg = SupervisorMessage;
    type State = RootSupervisorState;
    type Arguments = RootSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: RootSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        // Cassini-only -- this agent doesn't connect to the graph.
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

        let events_output: Arc<OutputPort<ClientEvent>> = Arc::new(OutputPort::default());
        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        let config = TCPClientConfig::new()?;
        let (tcp_client, _) = Actor::spawn_linked(
            Some(format!("{SERVICE_NAME}.tcp")),
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
            repo_path: args.repo_path,
            tcp_client,
            watcher: None,
            healthcheck,
            draining: false,
        })
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

                    if state.watcher.is_none() {
                        match Actor::spawn_linked(
                            Some("git-watcher".to_string()),
                            GitWatcherActor,
                            (state.repo_path.clone(), state.tcp_client.clone()),
                            myself.get_cell(),
                        )
                        .await
                        {
                            Ok((watcher, _)) => {
                                info!("Client registered, triggering initial scan");
                                watcher.cast(GitWatcherMsg::PerformInitialScan)?;
                                state.watcher = Some(watcher);
                            }
                            Err(e) => {
                                error!("Failed to spawn git watcher: {e}");
                                return Err(ActorProcessingErr::from(e));
                            }
                        }
                    } else {
                        debug!("Watcher already running; skipping respawn on reconnect");
                    }
                }
                // BUG FIX: previously this agent had no drain-aware
                // handling and no distinct branch for unexpected messages
                // -- not a panic risk here specifically, but flagged for
                // consistency with every other agent's pattern.
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniDisconnected);
                }
                _ => (),
            },
        }
        Ok(())
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
                warn!(
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
                // BUG FIX: previously, a watcher or tcp_client failure here
                // was either invisible (they were unlinked, untracked
                // top-level actors) or, for the watcher specifically, only
                // caught indirectly via watcher_handle.await? in main --
                // which worked but gave no chance to log or drain first.
                // Now routed through the same bounded drain window as
                // every other agent.
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    polar::init_logging(SERVICE_NAME.to_string());

    let config = ObserverConfig::from_env();

    // If remote URL is set, ensure the local repo exists.
    if let Some(remote_url) = &config.remote_url {
        let local_path = config.local_path.clone();
        let remote_url = remote_url.clone();
        let username = config.git_username.clone();
        let password = config.git_password.clone();

        // Clone if needed (blocking, but only at startup)
        tokio::task::spawn_blocking(move || {
            ensure_repo(
                &local_path,
                &remote_url,
                username.as_deref(),
                password.as_deref(),
            )
        })
        .await
        .map_err(|e| format!("Failed to ensure repo: {}", e))??;

        // If sync interval is set, spawn the sync actor
        if let Some(interval) = config.sync_interval {
            let (_, _) = Actor::spawn(
                Some("git-sync".to_string()),
                GitSyncActor,
                (
                    config.local_path.clone(),
                    config.git_username.clone(),
                    config.git_password.clone(),
                    interval,
                ),
            )
            .await?;
            // We don't need to keep the handle; it runs independently.
            // NOTE: this remains unlinked to RootSupervisor, same as
            // before -- a GitSyncActor failure is still invisible today.
            // Not addressed in this pass; flagged for the later
            // integration pass this agent needs.
        }
    } else {
        info!("No remote URL configured; watching local directory only.");
    }

    let repo_path = config
        .local_path
        .to_str()
        .expect("Invalid path")
        .to_string();

    let (_root, root_handle) = Actor::spawn(
        Some(format!("{SERVICE_NAME}.supervisor")),
        RootSupervisor,
        RootSupervisorArgs { repo_path },
    )
    .await?;

    root_handle.await?;
    Ok(())
}
