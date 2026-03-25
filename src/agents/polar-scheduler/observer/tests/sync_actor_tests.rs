use polar_scheduler_observer::sync_actor::{GitSyncActor, SyncMessage};
use ractor::Actor;
use std::time::Duration;
use tempfile::TempDir;

// No-op mock for TcpClientMessage dependency — not needed by GitSyncActor
// but kept here as a reference. GitSyncActor only needs (PathBuf, Option<String>,
// Option<String>, Duration) — no actor dependencies.

#[tokio::test]
async fn test_sync_actor_spawns() {
    let dir = TempDir::new().unwrap();
    let (actor, handle) = Actor::spawn(
        None,
        GitSyncActor,
        (
            dir.path().to_path_buf(),
            None::<String>,
            None::<String>,
            Duration::from_secs(3600),
        ),
    )
    .await
    .expect("GitSyncActor should spawn");

    actor.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_sync_actor_receives_tick_missing_repo() {
    // Tick on a nonexistent repo logs an error but does not crash the actor
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("no-such-repo");

    let (actor, handle) = Actor::spawn(
        None,
        GitSyncActor,
        (
            nonexistent,
            None::<String>,
            None::<String>,
            Duration::from_secs(3600),
        ),
    )
    .await
    .expect("GitSyncActor should spawn");

    actor.cast(SyncMessage::Tick).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Actor should still be alive after a failed tick
    actor.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_sync_actor_multiple_ticks_no_crash() {
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("no-such-repo");

    let (actor, handle) = Actor::spawn(
        None,
        GitSyncActor,
        (
            nonexistent,
            None::<String>,
            None::<String>,
            Duration::from_secs(3600),
        ),
    )
    .await
    .unwrap();

    for _ in 0..5 {
        actor.cast(SyncMessage::Tick).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    actor.stop(None);
    handle.await.unwrap();
}
