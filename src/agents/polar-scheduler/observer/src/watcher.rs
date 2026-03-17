use cassini_client::TcpClientMessage;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use polar_scheduler_common::GitScheduleChange;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::path::Path;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

pub struct GitWatcherActor;

#[derive(Clone)]
pub struct GitWatcherState {
    tcp_client: ActorRef<TcpClientMessage>,
    repo_path: String,
}

impl GitWatcherActor {
    fn evaluate_dhall_file_to_json(file_path: &Path) -> Result<String, ActorProcessingErr> {
        let simple: serde_dhall::SimpleValue = serde_dhall::from_file(file_path)
            .parse()
            .map_err(|e| ActorProcessingErr::from(format!("Dhall error: {}", e)))?;
        let json = serde_json::to_value(&simple)
            .map_err(|e| ActorProcessingErr::from(format!("JSON conversion error: {}", e)))?;
        Ok(serde_json::to_string(&json)?)
    }

    fn is_schedule_file(path: &Path, repo_path: &str) -> bool {
        let repo_path = Path::new(repo_path);
        let relative = path.strip_prefix(repo_path).unwrap_or(path);
        relative.components().any(|c| {
            let s = c.as_os_str().to_string_lossy();
            s == "permanent" || s == "adhoc" || s == "ephemeral"
        })
    }

    async fn start_filesystem_watcher(
        state: &GitWatcherState,
        myself: ActorRef<GitWatcherMsg>,
    ) -> Result<(), ActorProcessingErr> {
        let (tx, mut rx) = mpsc::channel(100);
        let path = state.repo_path.clone();

        // Spawn a blocking thread for notify (it's synchronous)
        std::thread::spawn(move || {
            let mut watcher: RecommendedWatcher = Watcher::new(
                move |res: notify::Result<notify::Event>| {
                    if let Ok(event) = res {
                        let _ = tx.blocking_send(event);
                    }
                },
                notify::Config::default(),
            )
            .unwrap();
            watcher
                .watch(Path::new(&path), RecursiveMode::Recursive)
                .unwrap();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        });

        // Process filesystem events
        while let Some(event) = rx.recv().await {
            for path in event.paths {
                if path.extension().and_then(|s| s.to_str()) == Some("dhall")
                    && Self::is_schedule_file(&path, &state.repo_path)
                {
                    let path_str = path.to_string_lossy().to_string();

                    // Handle delete immediately
                    if event.kind.is_remove() {
                        let change = GitScheduleChange::Delete { path: path_str };
                        myself.cast(GitWatcherMsg::PublishChange(change))?;
                        continue;
                    }

                    // For create/modify, evaluate the file directly
                    if event.kind.is_create() || event.kind.is_modify() {
                        let path_clone = path.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            Self::evaluate_dhall_file_to_json(&path_clone)
                        })
                        .await;

                        match result {
                            Ok(Ok(json)) => {
                                let change = if event.kind.is_create() {
                                    GitScheduleChange::Create {
                                        path: path_str,
                                        json,
                                    }
                                } else {
                                    GitScheduleChange::Update {
                                        path: path_str,
                                        json,
                                    }
                                };
                                myself.cast(GitWatcherMsg::PublishChange(change))?;
                            }
                            Ok(Err(e)) => {
                                error!("Failed to evaluate Dhall file {}: {:?}", path_str, e);
                            }
                            Err(e) => error!("Task join error for {}: {}", path_str, e),
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn initial_scan(
        state: &GitWatcherState,
        myself: ActorRef<GitWatcherMsg>,
    ) -> Result<(), ActorProcessingErr> {
        info!("Performing initial scan of {}", state.repo_path);
        let walker = walkdir::WalkDir::new(&state.repo_path)
            .follow_links(true)
            .into_iter()
            .filter_entry(|e| {
                !e.file_name()
                    .to_str()
                    .map(|s| s.starts_with('.'))
                    .unwrap_or(false)
            });
        for entry in walker {
            let entry = entry?;
            if entry.file_type().is_file()
                && let Some(ext) = entry.path().extension()
                && ext == "dhall"
            {
                let path = entry.path().to_path_buf();
                if !Self::is_schedule_file(&path, &state.repo_path) {
                    continue;
                }
                let path_str = path.to_string_lossy().to_string();
                let path_clone = path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    Self::evaluate_dhall_file_to_json(&path_clone)
                })
                .await;
                match result {
                    Ok(Ok(json)) => {
                        let change = GitScheduleChange::Create {
                            path: path_str,
                            json,
                        };
                        myself.cast(GitWatcherMsg::PublishChange(change))?;
                    }
                    Ok(Err(e)) => {
                        error!("Failed to evaluate Dhall file {}: {:?}", path_str, e)
                    }
                    Err(e) => error!("Task join error for {}: {}", path_str, e),
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum GitWatcherMsg {
    PublishChange(GitScheduleChange),
    PerformInitialScan,
}

#[ractor::async_trait]
impl Actor for GitWatcherActor {
    type Msg = GitWatcherMsg;
    type State = GitWatcherState;
    type Arguments = (String, ActorRef<TcpClientMessage>);

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        (repo_path, tcp_client): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!("GitWatcher starting, watching {}", repo_path);
        Ok(GitWatcherState {
            tcp_client,
            repo_path,
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Start filesystem watcher in background
        let myself_clone = myself.clone();
        let state_clone = (state.tcp_client.clone(), state.repo_path.clone());
        tokio::spawn(async move {
            if let Err(e) = Self::start_filesystem_watcher(
                &GitWatcherState {
                    tcp_client: state_clone.0,
                    repo_path: state_clone.1,
                },
                myself_clone,
            )
            .await
            {
                error!("Filesystem watcher error: {:?}", e);
            }
        });

        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            GitWatcherMsg::PublishChange(change) => {
                let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&change)?.to_vec();
                state.tcp_client.cast(TcpClientMessage::Publish {
                    topic: "scheduler.in".to_string(),
                    payload,
                    trace_ctx: None,
                })?;
                debug!("Published schedule change");
            }
            GitWatcherMsg::PerformInitialScan => {
                let myself = _myself.clone();
                let state_clone = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = Self::initial_scan(&state_clone, myself).await {
                        error!("Initial scan error: {:?}", e);
                    }
                });
            }
        }
        Ok(())
    }
}
