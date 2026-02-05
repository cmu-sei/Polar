use cassini_client::TcpClientMessage;
use git2::{FetchOptions, Oid, RemoteCallbacks, Repository};
use git_agent_common::{
    GitRepositoryMessage, RepoId, RepoObservationConfig, GIT_REPO_CONFIG_REQUESTS,
};
use polar::GIT_REPOSITORIES_TOPIC;
use ractor::concurrency::{self, Duration};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use rkyv::{rancor, to_bytes, Archive, Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, instrument, trace, warn};
pub const SERVICE_NAME: &str = "polar.git.observer";
pub const REPO_SUPERVISOR_NAME: &str = "polar.git.observer.repo.supervisor";

pub mod supervisor;

#[instrument]
pub fn send_event(
    client: ActorRef<TcpClientMessage>,
    event: GitRepositoryMessage,
    topic: String,
) -> Result<(), ActorProcessingErr> {
    let payload = to_bytes::<rancor::Error>(&event)?.to_vec();
    trace!("Forwarding event {event:?} on topic {topic}");
    let message = TcpClientMessage::Publish { topic, payload };
    Ok(client.send_message(message)?)
}

/* ============================
 * Supervisor
 * ============================
 */

/// Messages the supervisor accepts.
///
/// The supervisor does *not* do git work.
/// It routes, spawns, and enforces invariants.
#[derive(Debug)]
pub enum SupervisorMsg {}

/// Git Observer (Supervisor + Per-Repo Workers)
///
/// This module implements a supervisor/worker actor model where:
/// - The supervisor enforces one worker per repo invariant
/// - Each worker owns exclusive access to a single git repository
/// - Git crawling is parallelized safely across repositories
///
/// This design intentionally avoids shared mutable state.
/// If you think you need locks here, you have already made a mistake.
pub struct GitRepoSupervisor;

/// Supervisor actor.
///
/// Owns the worker registry.
/// This is the only place allowed to create workers.
pub struct GitRepoSupervisorState {
    cache_root: PathBuf,
    /// Registry enforcing one worker per repo.
    workers: HashMap<RepoId, ActorRef<GitRepoWorkerMsg>>,
    tcp_client: ActorRef<TcpClientMessage>,
}

pub struct GitRepoSupervisorArgs {
    cache_root: PathBuf,
    tcp_client: ActorRef<TcpClientMessage>,
}

/// Messages the supervisor accepts.
///
/// The supervisor does *not* do git work.
/// It routes, spawns, and enforces invariants.
#[derive(Serialize, Deserialize, Archive, Debug)]
pub enum RepoSupervisorMessage {
    ObserveRepo {
        repo_url: String,
    },
    SpawnWorker {
        config: RepoObservationConfig,
    },
    /// A stop message to trigger workspace cleanup logic. We don't want to keep repos and workers around indefinitely.
    StopWorker {
        repo_id: RepoId,
    },
}

#[async_trait]
impl Actor for GitRepoSupervisor {
    type Msg = RepoSupervisorMessage;
    type State = GitRepoSupervisorState;
    type Arguments = GitRepoSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitRepoSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!(
            "{myself:?} starting. Creating directories at {:?}",
            args.cache_root
        );

        // attempt to read cache root from environment variable and set up filesystem
        fs::create_dir_all(&args.cache_root)
            .map_err(|e| ActorProcessingErr::from(e.to_string()))?;

        Ok(GitRepoSupervisorState {
            cache_root: args.cache_root,
            tcp_client: args.tcp_client,
            workers: HashMap::new(),
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // TODO: REMOVE THIS AFTER TESTING
        myself.send_after(concurrency::Duration::from_secs(3), || {
            debug!("Observing repo");
            RepoSupervisorMessage::ObserveRepo {
                repo_url: "https://github.com/cmu-sei/Polar.git".to_string(),
            }
        });

        Ok(())
    }
    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            RepoSupervisorMessage::ObserveRepo { repo_url } => {
                let repo_id = RepoId::from_url(&repo_url);

                if let Some(worker) = state.workers.get(&repo_id) {
                    trace!(
                        "Forwarding observe request to worker for repo {}",
                        repo_id.to_string()
                    );
                    // Forward work to the worker.
                    worker.send_message(GitRepoWorkerMsg::Observe)?;
                } else {
                    // TODO: Before starting a worker, send a request to see if there's some schedule out there for this repo\
                    info!(
                        "No worker found for repo {}, requesting configuration",
                        repo_id.to_string()
                    );
                    let request = GitRepositoryMessage::ConfigurationRequest {
                        repo_url: repo_url.clone(),
                    };

                    send_event(
                        state.tcp_client.clone(),
                        request,
                        GIT_REPO_CONFIG_REQUESTS.to_string(),
                    )?;
                }
            }
            RepoSupervisorMessage::SpawnWorker { config } => {
                trace!("Received SpawnWorker message");
                let repo_id = RepoId::from_url(&config.repo_url);

                let args = GitRepoWorkerArgs {
                    repo_id: repo_id.clone(),
                    config,
                    repo_path: state.cache_root.join(&repo_id.to_string()),
                    tcp_client: state.tcp_client.clone(),
                };

                debug!("Starting worker for repo {}", repo_id.to_string());

                let (worker, _) = Actor::spawn_linked(
                    Some(format!("{SERVICE_NAME}.{}.worker", repo_id.to_string())),
                    GitRepoWorker,
                    args,
                    myself.clone().into(),
                )
                .await?;

                state.workers.insert(repo_id, worker);
            }
            RepoSupervisorMessage::StopWorker { repo_id } => {
                trace!("Received stop message for repo {}", repo_id.to_string());
                if let Some(worker) = state.workers.remove(&repo_id) {
                    info!("Stopping worker for repo {}", repo_id.to_string());
                    worker
                        .stop_and_wait(None, Some(Duration::from_secs(300)))
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorFailed(name, reason) => {
                error!("Actor {name:?} failed! {reason:?}");
                myself.stop(Some(reason.to_string()));
            }
            SupervisionEvent::ActorTerminated(name, _state, reason) => {
                warn!("Actor {name:?} terminated! {reason:?}");
                myself.stop(reason)
            }
            _ => {}
        }
        Ok(())
    }
}

/* ============================
 * Worker
 * ============================
 */

/// Messages a per-repo worker accepts.
#[derive(Serialize, Deserialize, Archive, Debug)]
pub enum GitRepoWorkerMsg {
    /// Fetch + export new data incrementally.
    /// Sent by the supervisor as a basic trigger.
    Observe,
}

pub struct GitRepoWorker;

/// Per-repo worker actor.
///
/// This actor owns:
/// - A single repo directory
/// - The git Repository handle
/// - Repo-local checkpoints (`last_seen`)
///
/// No other actor is allowed to touch this repo path.
pub struct GitRepoWorkerState {
    repo_id: RepoId,
    config: RepoObservationConfig,
    tcp_client: ActorRef<TcpClientMessage>,
    repo: Repository,
    /// Per-ref last exported commit.
    ///
    /// This MUST be persisted.
    last_seen: HashMap<String, Oid>,
}

pub struct GitRepoWorkerArgs {
    repo_id: RepoId,
    config: RepoObservationConfig,
    repo_path: PathBuf,
    tcp_client: ActorRef<TcpClientMessage>,
}

impl GitRepoWorker {
    /// Open an existing bare repository or clone a new one as bare.
    ///
    /// - No working tree
    /// - No checkout
    /// - No index
    ///
    /// This dramatically reduces filesystem churn and makes
    /// concurrent observation viable.
    pub fn open_or_clone_bare(repo_path: &Path, repo_url: &str) -> Result<Repository, git2::Error> {
        if repo_path.exists() {
            // SAFETY: Repository::open_bare enforces that this is bare.
            Repository::open_bare(repo_path)
        } else {
            let mut builder = git2::build::RepoBuilder::new();
            builder.bare(true);
            builder.clone(repo_url, repo_path)
        }
    }

    /// Fetch updates from `origin`, optionally with a shallow depth.
    ///
    /// Shallow fetches are useful for tip-only observation but
    /// explicitly trade away ancestry completeness.
    ///
    /// If `depth` is None, this performs a normal incremental fetch.
    /// If `depth` is Some(N), this performs a depth-limited fetch.
    pub fn fetch_with_optional_depth(
        repo: &Repository,
        remotes: &[String],
        depth: Option<usize>,
    ) -> Result<(), git2::Error> {
        if remotes.is_empty() {
            // No fetch configured — valid and intentional.
            return Ok(());
        }

        let mut callbacks = RemoteCallbacks::new();
        callbacks.credentials(|_url, _username, _allowed| git2::Cred::default());

        let mut fetch_opts = FetchOptions::new();
        fetch_opts.remote_callbacks(callbacks);

        if let Some(d) = depth {
            fetch_opts.depth(d as i32);
        }

        for remote_name in remotes {
            trace!("Attempting to find remote {remote_name}");
            let mut remote = repo.find_remote(remote_name)?;
            remote.fetch(&[] as &[&str], Some(&mut fetch_opts), None)?;
        }

        Ok(())
    }

    /// Walk commits for a single ref incrementally.
    ///
    /// The walk is bounded by:
    /// - `last_seen` checkpoint (if present)
    /// - `max_depth` hard limit
    ///
    /// This guarantees:
    /// - No unbounded history traversal
    /// - Deterministic cost per observation cycle
    pub fn walk_commits_incremental<F>(
        repo: &Repository,
        ref_name: &str,
        last_seen: Option<Oid>,
        max_depth: usize,
        mut on_commit: F,
    ) -> Result<Option<Oid>, git2::Error>
    where
        F: FnMut(&git2::Commit),
    {
        let reference = repo.find_reference(ref_name)?;
        let tip = reference
            .target()
            .ok_or_else(|| git2::Error::from_str("ref has no target"))?;

        let mut revwalk = repo.revwalk()?;
        revwalk.push(tip)?;

        // If we have a checkpoint, hide everything reachable from it.
        if let Some(stop) = last_seen {
            // hide() is best-effort; failure here is non-fatal.
            let _ = revwalk.hide(stop);
        }

        let mut depth = 0;
        let mut newest_seen: Option<Oid> = None;

        for oid in revwalk {
            if depth >= max_depth {
                break;
            }

            let oid = oid?;
            let commit = repo.find_commit(oid)?;

            // IMPORTANT:
            // We never touch trees or blobs.
            // Reading commit headers is cheap and safe.
            on_commit(&commit);

            if newest_seen.is_none() {
                newest_seen = Some(oid);
            }

            depth += 1;
        }

        Ok(newest_seen)
    }

    /// Observe a repository according to an explicit policy.
    ///
    /// This function:
    /// - assumes a bare repository
    /// - performs a bounded fetch
    /// - walks only configured refs
    /// - updates per-ref checkpoints
    ///
    /// It does NOT:
    /// - schedule itself
    /// - persist checkpoints
    /// - spawn threads or actors
    pub fn observe_repository<F>(
        repo: &Repository,
        config: &RepoObservationConfig,
        last_seen: &mut HashMap<String, Oid>,
        mut on_commit: F,
    ) -> Result<(), git2::Error>
    where
        F: FnMut(&str, &git2::Commit),
    {
        // Fetch first; observation is always against the freshest refs.
        Self::fetch_with_optional_depth(repo, &config.remotes, config.shallow_depth)?;

        for ref_name in &config.refs {
            let prev = last_seen.get(ref_name).copied();

            let newest =
                Self::walk_commits_incremental(repo, ref_name, prev, config.max_depth, |commit| {
                    on_commit(ref_name, commit);
                })?;

            if let Some(oid) = newest {
                last_seen.insert(ref_name.to_string(), oid);
            }
        }

        Ok(())
    }
}

fn on_commit(
    repo_id: RepoId,
    ref_name: &str,
    commit: &git2::Commit,
    tcp_client: ActorRef<TcpClientMessage>,
) -> Result<(), ActorProcessingErr> {
    debug!("Found commit for ref_name {ref_name} in repo {repo_id:?}");
    let event = GitRepositoryMessage::Commit {
        repo: repo_id,
        oid: commit.id().to_string(),
        author: commit.author().name().unwrap_or_default().to_string(),
        time: commit.time().seconds(),
        message: commit.message().unwrap_or_default().to_string(),
        parents: commit.parent_ids().map(|id| id.to_string()).collect(),
    };

    debug!("Emitting event {:?}", event);
    //TODO: actually emit event over the wire
    // send_event(tcp_client, event, GIT_REPOSITORIES_TOPIC.to_string())?;

    Ok(())
}

#[async_trait]
impl Actor for GitRepoWorker {
    type Msg = GitRepoWorkerMsg;
    type State = GitRepoWorkerState;
    type Arguments = GitRepoWorkerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitRepoWorkerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        // Open or clone repo during actor startup.
        let repo = Self::open_or_clone_bare(&args.repo_path, &args.config.repo_url)?;
        // TODO: Load persisted last_seen checkpoints here.

        Ok(GitRepoWorkerState {
            repo_id: args.repo_id,
            config: args.config,
            tcp_client: args.tcp_client,
            repo,
            last_seen: HashMap::new(),
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} started");

        // send self an Observe message on startup once we've properly cloned or opened the repo.
        myself.cast(GitRepoWorkerMsg::Observe)?;
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            GitRepoWorkerMsg::Observe => {
                trace!("Received Observe message");

                let mut message_send_failed = false;
                Self::observe_repository(
                    &state.repo,
                    &state.config,
                    &mut state.last_seen,
                    |ref_name, commit| {
                        message_send_failed = on_commit(
                            state.repo_id.clone(),
                            ref_name,
                            commit,
                            state.tcp_client.clone(),
                        )
                        .is_err();
                    },
                )?;

                if message_send_failed {
                    return Err(ActorProcessingErr::from("Failed to send commit"));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod unittests {
    use super::*;
    use git2::{Commit, Oid, Repository};
    use std::collections::HashMap;
    use std::sync::Once;
    use tempfile::TempDir;

    static INIT_LOGGING: Once = Once::new();

    fn init_logging() {
        INIT_LOGGING.call_once(|| {
            polar::init_logging(crate::SERVICE_NAME.to_string());
        });
    }

    fn dummy_config(refs: Vec<String>) -> RepoObservationConfig {
        RepoObservationConfig::new("local".into(), Vec::new(), Some(100), refs)
    }

    /// Create a bare repository for testing.
    fn bare_repo() -> (TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init_bare(dir.path()).unwrap();

        (dir, repo)
    }

    /// Create an empty-tree commit with optional parents.
    fn make_commit(repo: &Repository, message: &str, parents: &[&Commit]) -> Oid {
        let sig = git2::Signature::now("tester", "tester@example.com").unwrap();

        let tree_id = repo.treebuilder(None).unwrap().write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        repo.commit(None, &sig, &sig, message, &tree, parents)
            .unwrap()
    }

    /// Create a linear chain of commits and point a ref at the tip.
    /// Returns commits in creation order (oldest → newest).
    fn linear_history(repo: &Repository, ref_name: &str, count: usize) -> Vec<Oid> {
        let mut commits = Vec::new();

        for i in 0..count {
            let parents = commits
                .last()
                .and_then(|oid| repo.find_commit(*oid).ok())
                .into_iter()
                .collect::<Vec<_>>();

            let oid = make_commit(repo, &format!("c{i}"), &parents.iter().collect::<Vec<_>>());

            commits.push(oid);
        }

        repo.reference(ref_name, *commits.last().unwrap(), true, "init")
            .unwrap();

        commits
    }

    #[test]
    fn walk_without_last_seen_returns_newest_first() {
        init_logging();
        let (_dir, repo) = bare_repo();
        let commits = linear_history(&repo, "refs/heads/main", 3);

        let mut seen = Vec::new();

        GitRepoWorker::walk_commits_incremental(&repo, "refs/heads/main", None, 10, |c| {
            seen.push(c.id())
        })
        .unwrap();

        assert_eq!(seen, commits.iter().rev().copied().collect::<Vec<_>>());
    }

    #[test]
    fn walk_respects_max_depth() {
        init_logging();
        let (_dir, repo) = bare_repo();
        let commits = linear_history(&repo, "refs/heads/main", 5);

        let mut seen = Vec::new();

        GitRepoWorker::walk_commits_incremental(&repo, "refs/heads/main", None, 2, |c| {
            seen.push(c.id())
        })
        .unwrap();

        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], commits[4]);
        assert_eq!(seen[1], commits[3]);
    }

    #[test]
    fn walk_nonexistent_ref_errors_cleanly() {
        init_logging();
        let (_dir, repo) = bare_repo();

        let err =
            GitRepoWorker::walk_commits_incremental(&repo, "refs/heads/nope", None, 10, |_| {})
                .err()
                .expect("expected error");

        assert!(
            err.message().contains("reference"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn observe_updates_last_seen_per_ref_independently() {
        init_logging();
        let (_dir, repo) = bare_repo();

        let main_commits = linear_history(&repo, "refs/heads/main", 2);
        let dev_commits = linear_history(&repo, "refs/heads/dev", 3);

        let mut last_seen = HashMap::new();
        let mut seen = Vec::new();

        let config = dummy_config(vec!["refs/heads/main".into(), "refs/heads/dev".into()]);

        GitRepoWorker::observe_repository(&repo, &config, &mut last_seen, |r, c| {
            seen.push((r.to_string(), c.id()))
        })
        .unwrap();

        assert_eq!(last_seen["refs/heads/main"], *main_commits.last().unwrap());
        assert_eq!(last_seen["refs/heads/dev"], *dev_commits.last().unwrap());
    }

    #[test]
    fn multiple_refs_do_not_interfere() {
        init_logging();
        let (_dir, repo) = bare_repo();

        let main_commits = linear_history(&repo, "refs/heads/main", 1);
        let dev_commits = linear_history(&repo, "refs/heads/dev", 1);

        let mut last_seen = HashMap::from([
            ("refs/heads/main".into(), main_commits[0]),
            ("refs/heads/dev".into(), dev_commits[0]),
        ]);

        let new_dev = make_commit(
            &repo,
            "dev-2",
            &[&repo.find_commit(dev_commits[0]).unwrap()],
        );
        repo.reference("refs/heads/dev", new_dev, true, "update")
            .unwrap();

        let config = dummy_config(vec!["refs/heads/dev".into()]);

        GitRepoWorker::observe_repository(&repo, &config, &mut last_seen, |_r, _c| {}).unwrap();

        assert_eq!(last_seen["refs/heads/dev"], new_dev);
        assert_eq!(last_seen["refs/heads/main"], main_commits[0]);
    }

    #[test]
    fn empty_repo_is_noop_not_panic() {
        init_logging();
        let (_dir, repo) = bare_repo();

        let mut last_seen = HashMap::new();
        let config = dummy_config(vec!["refs/heads/main".into()]);

        let result = GitRepoWorker::observe_repository(&repo, &config, &mut last_seen, |_r, _c| {});

        assert!(result.is_err() || last_seen.is_empty());
    }
}
