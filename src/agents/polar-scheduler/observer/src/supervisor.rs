//! Supervisor for the scheduler observer, on the agent builder: health check, router and the
//! Cassini client come from `AgentBuilder`.
//!
//! Children: the sync actor (when there is a remote), which pulls on an interval and whenever the
//! Polar git server announces a change to this repository; and the watcher, which publishes each
//! schedule change in the checkout. Both are linked, so a failure of either ends the process and
//! the Job starts a fresh one.
//!
//! `Registered` fires on every registration, a reconnect included, so it only reports health and
//! starts the watcher if there is none.
//!
//! Shutdown, in order, when the health check ends the pod's life:
//!
//! 1. `InfrastructureEvent::PrepareShutdown` reaches this supervisor.
//! 2. The sync actor and the watcher are stopped.
//! 3. `ProceedWithShutdown` goes to the infrastructure supervisor, which stops its children.
//! 4. The infrastructure supervisor's termination makes this supervisor exit non-zero, so the
//!    workload is restarted with fresh certificates.

use crate::git_ops::{GitTls, ensure_repo, repo_name};
use crate::settings::ObserverSettings;
use crate::sync_actor::{GitSyncActor, GitSyncArgs, SyncMessage};
use crate::watcher::{GitWatcherActor, GitWatcherMsg};
use cassini_client::TcpClientMessage;
use cassini_types::ClientEvent;
use polar_agent_builder::{
    AgentBuilder, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage, Subscription,
};
use polar_git_server_common::{POLAR_GIT_REPO_CHANGED_TOPIC, RepoChangedEvent};
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use std::path::PathBuf;
use std::time::Duration;
use tracing::{debug, error, info, warn};

pub const SERVICE_NAME: &str = "polar-scheduler-observer";

/// Stopping two small actors is all there is to drain; the rest is margin.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

pub enum SchedulerObserverMessage {
    ClientEvent {
        event: ClientEvent,
    },
    /// The Polar git server says a ref of one of its repositories moved.
    RepoChanged(RepoChangedEvent),
    PrepareShutdown,
}

/// A change announced for another repository is not about this checkout.
pub fn concerns(event: &RepoChangedEvent, watched: Option<&str>) -> bool {
    watched == Some(event.repo.as_str())
}

/// The subscription to the git server's announcements: each is handed to the supervisor, which
/// decides whether it concerns this checkout.
pub fn repo_changed_subscription(supervisor: ActorRef<SchedulerObserverMessage>) -> Subscription {
    Subscription::new(
        POLAR_GIT_REPO_CHANGED_TOPIC,
        move |incoming| match rkyv::from_bytes::<RepoChangedEvent, rkyv::rancor::Error>(
            &incoming.payload,
        ) {
            Ok(event) => {
                let _ = supervisor.cast(SchedulerObserverMessage::RepoChanged(event));
            }
            Err(e) => warn!("failed to deserialize a repository change: {e}"),
        },
    )
}

pub struct SchedulerObserverSupervisor;

pub struct SchedulerObserverSupervisorState {
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    tcp_client: ActorRef<TcpClientMessage>,
    repo_path: String,
    /// The repository this checkout is of, when it has a remote.
    watched_repo: Option<String>,
    sync: Option<ActorRef<SyncMessage>>,
    watcher: Option<ActorRef<GitWatcherMsg>>,
}

#[async_trait]
impl Actor for SchedulerObserverSupervisor {
    type Msg = SchedulerObserverMessage;
    type State = SchedulerObserverSupervisorState;
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!("{myself:?} starting");

        let settings = ObserverSettings::read(configured.config())?;
        let resources = AgentBuilder::new(myself.clone(), SERVICE_NAME)
            .with_lifecycle_handler(|event| Some(SchedulerObserverMessage::ClientEvent { event }))
            .with_subscription(repo_changed_subscription(myself.clone()))
            .into_configured(configured)
            .build()
            .await?;

        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => {
                    Some(SchedulerObserverMessage::PrepareShutdown)
                }
            });

        let tls = GitTls {
            ca_cert: settings.git_ca_cert.map(PathBuf::from),
            client_cert: settings.git_client_cert.map(PathBuf::from),
            client_key: settings.git_client_key.map(PathBuf::from),
        };

        let mut sync = None;
        let mut watched_repo = None;
        if let Some(remote_url) = &settings.remote_url {
            // The first clone is blocking and the observer has nothing to watch without it.
            let (path, url, clone_tls) = (
                PathBuf::from(&settings.local_path),
                remote_url.clone(),
                tls.clone(),
            );
            tokio::task::spawn_blocking(move || ensure_repo(&path, &url, &clone_tls))
                .await
                .map_err(|e| format!("cloning the schedules repository panicked: {e}"))?
                .map_err(|e| format!("cannot get the schedules repository: {e:#}"))?;

            watched_repo = repo_name(remote_url).map(str::to_string);
            let interval = settings
                .sync_interval_secs
                .filter(|secs| *secs > 0)
                .map(Duration::from_secs);
            let (actor, _) = Actor::spawn_linked(
                Some(format!("{SERVICE_NAME}.sync")),
                GitSyncActor,
                GitSyncArgs {
                    repo_path: PathBuf::from(&settings.local_path),
                    tls,
                    interval,
                },
                myself.get_cell(),
            )
            .await?;
            sync = Some(actor);
        } else {
            info!("No remote URL configured; watching local directory only.");
        }

        Ok(SchedulerObserverSupervisorState {
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            tcp_client: resources.tcp_client.as_actor_ref().clone(),
            repo_path: settings.local_path,
            watched_repo,
            sync,
            watcher: None,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SchedulerObserverMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cassini_connected();
                    if state.watcher.is_none() {
                        let (watcher, _) = Actor::spawn_linked(
                            Some("git-watcher".to_string()),
                            GitWatcherActor,
                            (state.repo_path.clone(), state.tcp_client.clone()),
                            myself.get_cell(),
                        )
                        .await?;
                        info!("Client registered, triggering initial scan");
                        watcher.cast(GitWatcherMsg::PerformInitialScan)?;
                        state.watcher = Some(watcher);
                    } else {
                        debug!("Watcher already running; not starting another on reconnect");
                    }
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => {}
            },
            SchedulerObserverMessage::RepoChanged(event) => {
                if !concerns(&event, state.watched_repo.as_deref()) {
                    debug!(repo = %event.repo, "change is for another repository");
                } else if let Some(sync) = &state.sync {
                    info!(repo = %event.repo, refname = %event.refname, pusher = %event.pusher, "repository changed; pulling");
                    sync.cast(SyncMessage::Now)?;
                }
            }
            SchedulerObserverMessage::PrepareShutdown => {
                info!(
                    "PrepareShutdown received: stopping the watcher and the sync before infrastructure"
                );
                if let Some(sync) = state.sync.take() {
                    sync.stop(None);
                }
                if let Some(watcher) = state.watcher.take() {
                    watcher.stop(None);
                }
                let _ = state
                    .infrastructure
                    .cast(InfrastructureSupervisorMessage::ProceedWithShutdown);
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisionEvent::ActorTerminated(actor, _, reason) => {
                if actor.get_id() == state.infrastructure.get_id() {
                    info!("infrastructure shutdown complete ({reason:?}), exiting");
                    std::process::exit(1);
                }
            }
            SupervisionEvent::ActorFailed(actor, err) => {
                error!("Child actor {actor:?} failed: {err}; exiting");
                std::process::exit(1);
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(repo: &str) -> RepoChangedEvent {
        RepoChangedEvent {
            repo: repo.into(),
            refname: "refs/heads/main".into(),
            old: "a".repeat(40),
            new: "b".repeat(40),
            pusher: "alice".into(),
        }
    }

    #[test]
    fn only_a_change_to_the_watched_repository_concerns_the_checkout() {
        assert!(concerns(&event("polar-schedules"), Some("polar-schedules")));
        assert!(!concerns(&event("other"), Some("polar-schedules")));
        assert!(!concerns(&event("polar-schedules"), None));
    }

    #[test]
    fn subscribes_to_the_git_servers_topic() {
        struct Sink;
        #[ractor::async_trait]
        impl Actor for Sink {
            type Msg = SchedulerObserverMessage;
            type State = ();
            type Arguments = ();
            async fn pre_start(
                &self,
                _: ActorRef<Self::Msg>,
                _: (),
            ) -> Result<(), ActorProcessingErr> {
                Ok(())
            }
        }
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (sink, _) = Actor::spawn(None, Sink, ()).await.unwrap();
            let sub = repo_changed_subscription(sink);
            assert_eq!(sub.topic, POLAR_GIT_REPO_CHANGED_TOPIC);
            // A payload that is not an event is dropped without a panic.
            (sub.deliver)(polar_router::IncomingMessage {
                topic: sub.topic.clone(),
                payload: vec![1, 2, 3],
                published_by: None,
                trace_ctx: None,
            });
        });
    }
}
