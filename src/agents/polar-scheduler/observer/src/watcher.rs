use cassini_client::TcpClientMessage;
use polar_scheduler_common::GitScheduleChange;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
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
                        // Send event path to actor
                        let _ = tx.blocking_send(event);
                    }
                },
                notify::Config::default(),
            ).unwrap();
            watcher.watch(Path::new(&path), RecursiveMode::Recursive).unwrap();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        });

        // Process filesystem events
        while let Some(event) = rx.recv().await {
            for path in event.paths {
                if path.extension().and_then(|s| s.to_str()) == Some("dhall") {
                    let path_str = path.to_string_lossy().to_string();
                    let change = if event.kind.is_create() {
                        // Read file content
                        if let Ok(content) = tokio::fs::read(&path).await {
                            GitScheduleChange::Create { path: path_str, content }
                        } else {
                            continue;
                        }
                    } else if event.kind.is_modify() {
                        if let Ok(content) = tokio::fs::read(&path).await {
                            GitScheduleChange::Update { path: path_str, content }
                        } else {
                            continue;
                        }
                    } else if event.kind.is_remove() {
                        GitScheduleChange::Delete { path: path_str }
                    } else {
                        continue;
                    };
                    // Send change to self for publishing
                    myself.cast(GitWatcherMsg::PublishChange(change))?;
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
            .filter_entry(|e| !e.file_name().to_str().map(|s| s.starts_with('.')).unwrap_or(false));
        for entry in walker {
            let entry = entry?;
            if entry.file_type().is_file() {
                if let Some(ext) = entry.path().extension() {
                    if ext == "dhall" {
                        let path = entry.path().to_path_buf();
                        let path_str = path.to_string_lossy().to_string();
                        if let Ok(content) = tokio::fs::read(&path).await {
                            let change = GitScheduleChange::Create { path: path_str, content };
                            myself.cast(GitWatcherMsg::PublishChange(change))?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum GitWatcherMsg {
    PublishChange(GitScheduleChange),
}

#[ractor::async_trait]
impl Actor for GitWatcherActor {
    type Msg = GitWatcherMsg;
    type State = GitWatcherState;
    type Arguments = (String, ActorRef<TcpClientMessage>); // (repo_path, tcp_client)

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        (repo_path, tcp_client): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!("GitWatcher starting, watching {}", repo_path);
        Ok(GitWatcherState { tcp_client, repo_path })
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
                &GitWatcherState { tcp_client: state_clone.0, repo_path: state_clone.1 },
                myself_clone,
            ).await {
                error!("Filesystem watcher error: {:?}", e);
            }
        });

        // Perform initial scan
        let myself_clone = myself.clone();
        let state_clone = state.clone(); // need to clone state; derive Clone for GitWatcherState
        tokio::spawn(async move {
            if let Err(e) = Self::initial_scan(&state_clone, myself_clone).await {
                error!("Initial scan error: {:?}", e);
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
        }
        Ok(())
    }
}
