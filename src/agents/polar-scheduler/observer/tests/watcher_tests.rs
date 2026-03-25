use cassini_client::TcpClientMessage;
use polar_scheduler_common::GitScheduleChange;
use polar_scheduler_observer::watcher::{GitWatcherActor, GitWatcherMsg};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::time::Duration;
use tempfile::TempDir;

struct MockTcpClient;

#[ractor::async_trait]
impl Actor for MockTcpClient {
    type Msg = TcpClientMessage;
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

#[tokio::test]
async fn test_watcher_spawns() {
    let dir = TempDir::new().unwrap();

    let (tcp, _tcp_handle) = Actor::spawn(None, MockTcpClient, ()).await.unwrap();
    let (watcher, handle) = Actor::spawn(
        None,
        GitWatcherActor,
        (dir.path().to_string_lossy().to_string(), tcp),
    )
    .await
    .expect("GitWatcherActor should spawn");

    watcher.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_watcher_publish_change_create() {
    let dir = TempDir::new().unwrap();

    let (tcp, _tcp_handle) = Actor::spawn(None, MockTcpClient, ()).await.unwrap();
    let (watcher, handle) = Actor::spawn(
        None,
        GitWatcherActor,
        (dir.path().to_string_lossy().to_string(), tcp),
    )
    .await
    .unwrap();

    let change = GitScheduleChange::Create {
        path: "permanent/test.dhall".to_string(),
        json: "{}".to_string(),
    };
    watcher.cast(GitWatcherMsg::PublishChange(change)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    watcher.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_watcher_publish_change_update() {
    let dir = TempDir::new().unwrap();

    let (tcp, _tcp_handle) = Actor::spawn(None, MockTcpClient, ()).await.unwrap();
    let (watcher, handle) = Actor::spawn(
        None,
        GitWatcherActor,
        (dir.path().to_string_lossy().to_string(), tcp),
    )
    .await
    .unwrap();

    let change = GitScheduleChange::Update {
        path: "permanent/test.dhall".to_string(),
        json: "{}".to_string(),
    };
    watcher.cast(GitWatcherMsg::PublishChange(change)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    watcher.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_watcher_publish_change_delete() {
    let dir = TempDir::new().unwrap();

    let (tcp, _tcp_handle) = Actor::spawn(None, MockTcpClient, ()).await.unwrap();
    let (watcher, handle) = Actor::spawn(
        None,
        GitWatcherActor,
        (dir.path().to_string_lossy().to_string(), tcp),
    )
    .await
    .unwrap();

    let change = GitScheduleChange::Delete {
        path: "permanent/test.dhall".to_string(),
    };
    watcher.cast(GitWatcherMsg::PublishChange(change)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    watcher.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_watcher_perform_initial_scan_empty_dir() {
    let dir = TempDir::new().unwrap();

    let (tcp, _tcp_handle) = Actor::spawn(None, MockTcpClient, ()).await.unwrap();
    let (watcher, handle) = Actor::spawn(
        None,
        GitWatcherActor,
        (dir.path().to_string_lossy().to_string(), tcp),
    )
    .await
    .unwrap();

    watcher.cast(GitWatcherMsg::PerformInitialScan).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    watcher.stop(None);
    handle.await.unwrap();
}
