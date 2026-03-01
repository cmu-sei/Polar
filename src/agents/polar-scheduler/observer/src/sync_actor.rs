use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::path::PathBuf;
use std::time::Duration;
use tracing::{error, info};

use crate::git_ops::pull_repo;

pub struct GitSyncActor;

pub struct GitSyncState {
    repo_path: PathBuf,
    username: Option<String>,
    password: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SyncMessage {
    Tick,
}

#[ractor::async_trait]
impl Actor for GitSyncActor {
    type Msg = SyncMessage;
    type State = GitSyncState;
    type Arguments = (PathBuf, Option<String>, Option<String>, Duration);

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        (repo_path, username, password, interval): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!("GitSyncActor starting, will pull every {}s", interval.as_secs());
        // Schedule the first tick immediately
        myself.send_interval(interval, || SyncMessage::Tick);
        Ok(GitSyncState {
            repo_path,
            username,
            password,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SyncMessage::Tick => {
                info!("GitSyncActor tick – pulling repo at {}", state.repo_path.display());
                let path = state.repo_path.clone();
                let username = state.username.clone();
                let password = state.password.clone();

                // Perform the blocking pull in a separate thread
                tokio::task::spawn_blocking(move || {
                    match git2::Repository::open(&path) {
                        Ok(repo) => {
                            if let Err(e) = pull_repo(&repo, username.as_deref(), password.as_deref()) {
                                error!("Failed to pull repo: {:?}", e);
                            } else {
                                info!("Pull completed successfully");
                            }
                        }
                        Err(e) => error!("Cannot open repo at {}: {:?}", path.display(), e),
                    }
                })
                .await
                .map_err(|e| ActorProcessingErr::from(e.to_string()))?;
            }
        }
        Ok(())
    }
}
