use polar_scheduler_observer::git_ops::{GitTls, ensure_repo};
use polar_scheduler_observer::sync_actor::{GitSyncActor, GitSyncArgs, SyncMessage};
use ractor::Actor;
use std::time::Duration;
use tempfile::TempDir;

#[tokio::test]
async fn test_sync_actor_spawns() {
    let dir = TempDir::new().unwrap();
    let (actor, handle) = Actor::spawn(
        None,
        GitSyncActor,
        GitSyncArgs {
            repo_path: dir.path().to_path_buf(),
            tls: GitTls::default(),
            interval: Some(Duration::from_secs(3600)),
        },
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
        GitSyncArgs {
            repo_path: nonexistent,
            tls: GitTls::default(),
            interval: Some(Duration::from_secs(3600)),
        },
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
        GitSyncArgs {
            repo_path: nonexistent,
            tls: GitTls::default(),
            interval: Some(Duration::from_secs(3600)),
        },
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

fn commit_file(repo: &git2::Repository, root: &std::path::Path, name: &str) {
    std::fs::write(root.join(name), "1").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new(name)).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("test", "test@test.com").unwrap();
    let parents: Vec<git2::Commit> = repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_commit().ok())
        .into_iter()
        .collect();
    let parents: Vec<&git2::Commit> = parents.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, "edit", &tree, &parents)
        .unwrap();
}

/// With no interval at all, a `Now` is what brings a new commit in: the path the git server's
/// announcement takes.
#[tokio::test]
async fn test_sync_actor_pulls_when_told_and_never_on_its_own() {
    let source = TempDir::new().unwrap();
    let repo = git2::Repository::init(source.path()).unwrap();
    commit_file(&repo, source.path(), "first.dhall");

    let work = TempDir::new().unwrap();
    let checkout = work.path().join("checkout");
    ensure_repo(
        &checkout,
        &format!("file://{}", source.path().display()),
        &GitTls::default(),
    )
    .unwrap();

    let (actor, handle) = Actor::spawn(
        None,
        GitSyncActor,
        GitSyncArgs {
            repo_path: checkout.clone(),
            tls: GitTls::default(),
            interval: None,
        },
    )
    .await
    .unwrap();

    commit_file(&repo, source.path(), "second.dhall");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !checkout.join("second.dhall").exists(),
        "nothing asked it to pull"
    );

    actor.cast(SyncMessage::Now).unwrap();
    for _ in 0..50 {
        if checkout.join("second.dhall").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        checkout.join("second.dhall").exists(),
        "Now should pull the new commit"
    );

    actor.stop(None);
    handle.await.unwrap();
}
