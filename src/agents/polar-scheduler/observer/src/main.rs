use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs};
use ractor::Actor;

mod watcher;
use watcher::GitWatcherActor;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    polar::init_logging("polar-scheduler-observer".to_string());

    let repo_path = std::env::var("GIT_REPO_PATH")
        .expect("GIT_REPO_PATH must be set");

    let config = TCPClientConfig::new()
        .expect("Failed to create TCP client config");
    let (tcp_client, _handle) = Actor::spawn(
        Some("observer.tcp".to_string()),
        TcpClientActor,
        TcpClientArgs {
            config,
            registration_id: None,
            events_output: None,
            event_handler: None,
        },
    )
    .await?;

    let (_watcher, watcher_handle) = Actor::spawn(
        Some("git-watcher".to_string()),
        GitWatcherActor,
        (repo_path, tcp_client),
    )
    .await?;

    watcher_handle.await?;
    Ok(())
}
