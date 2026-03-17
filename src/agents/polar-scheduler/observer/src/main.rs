mod config;
mod git_ops;
mod sync_actor;
mod watcher;

use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs};
use cassini_types::ClientEvent;
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort};
use std::sync::Arc;
use tracing::info;

use config::ObserverConfig;
use git_ops::ensure_repo;
use sync_actor::GitSyncActor;
use watcher::{GitWatcherActor, GitWatcherMsg};

// Actor that waits for registration and then triggers the initial scan
struct RegistrationWaiter;

#[ractor::async_trait]
impl Actor for RegistrationWaiter {
    type Msg = ClientEvent;
    type State = ActorRef<GitWatcherMsg>;
    type Arguments = ActorRef<GitWatcherMsg>;

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        watcher: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(watcher)
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let ClientEvent::Registered { .. } = msg {
            info!("Client registered, triggering initial scan");
            state.cast(GitWatcherMsg::PerformInitialScan)?;
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    polar::init_logging("polar-scheduler-observer".to_string());

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
            // We don't need to keep the handle; it runs independently
        }
    } else {
        info!("No remote URL configured; watching local directory only.");
    }

    let repo_path = config
        .local_path
        .to_str()
        .expect("Invalid path")
        .to_string();

    // Create a channel for client events (optional, but we need an OutputPort)
    let events_output: Arc<OutputPort<ClientEvent>> = Arc::new(OutputPort::default());

    // Connect to Cassini
    let tcp_config = TCPClientConfig::new().expect("Failed to create TCP client config");
    let (tcp_client, _handle) = Actor::spawn(
        Some("observer.tcp".to_string()),
        TcpClientActor,
        TcpClientArgs {
            config: tcp_config,
            registration_id: None,
            events_output: Some(events_output.clone()),
            event_handler: None,
        },
    )
    .await?;

    // Spawn the Git watcher actor
    let (watcher, watcher_handle) = Actor::spawn(
        Some("git-watcher".to_string()),
        GitWatcherActor,
        (repo_path, tcp_client),
    )
    .await?;

    // Spawn the registration waiter, subscribed to the output port
    let (waiter, _) = Actor::spawn(
        Some("registration-waiter".to_string()),
        RegistrationWaiter,
        watcher.clone(),
    )
    .await?;
    events_output.subscribe(waiter, Some);

    watcher_handle.await?;
    Ok(())
}
