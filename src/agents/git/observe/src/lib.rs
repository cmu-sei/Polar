use cassini_client::TcpClientMessage;
use git_agent_common::{GIT_REPO_PROCESSING_TOPIC, GitHttpCredential, GitRepositoryMessage, RepoObservationConfig};
use git2::{Cred, CredentialType, FetchOptions, Oid, RemoteCallbacks, Repository};
use polar::graph::nodes::git::RepoId;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use rkyv::{Archive, Deserialize, Serialize, rancor, to_bytes};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, error, instrument, trace, warn};

pub const SERVICE_NAME: &str = "polar.git.observer";
pub const REPO_SUPERVISOR_NAME: &str = "polar.git.observer.repo.supervisor";

pub mod supervisor;

/* ============================
 * Credential Provider
 * ============================
 */

/// A source of Git credentials.
///
/// This abstraction:
/// - Hides secret storage details from the worker
/// - Is compatible with git2's callback model
/// - Is unit testable via mock implementations
///
/// Implementations MUST be:
/// - Cheap to call (no blocking IO)
/// - Thread-safe
pub trait GitCredentialProvider: Send + Sync {
    /// Resolve credentials for a Git operation.
    ///
    /// Parameters are passed through from git2's credentials callback.
    ///
    /// # Arguments
    ///
    /// * `url` - Full remote URL
    /// * `username_from_url` - Optional username parsed from URL
    /// * `allowed` - Credential types git2 is willing to accept
    fn credentials(
        &self,
        url: &str,
        username_from_url: Option<&str>,
        allowed: CredentialType,
    ) -> Result<Cred, git2::Error>;
}

/// A credential provider backed by a `GitHttpCredential` value received
/// from the scheduler at task dispatch time.
///
/// This replaces the old `StaticCredentialProvider` which loaded credentials
/// from a file at agent startup. Credentials are now per-task, not per-agent.
pub struct TaskCredentialProvider {
    credential: GitHttpCredential,
}

impl TaskCredentialProvider {
    pub fn new(credential: GitHttpCredential) -> Self {
        Self { credential }
    }
}

impl GitCredentialProvider for TaskCredentialProvider {
    fn credentials(
        &self,
        _url: &str,
        _username_from_url: Option<&str>,
        allowed: CredentialType,
    ) -> Result<Cred, git2::Error> {
        if !allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
            return Err(git2::Error::from_str("unsupported credential type"));
        }
        self.credential.into_git2_cred()
    }
}

/// A no-op credential provider for public repositories.
///
/// Used when `RepoObservationConfig::credentials` is `None`.
/// Git2 will attempt unauthenticated access.
pub struct PublicRepoProvider;

impl GitCredentialProvider for PublicRepoProvider {
    fn credentials(
        &self,
        _url: &str,
        _username_from_url: Option<&str>,
        _allowed: CredentialType,
    ) -> Result<Cred, git2::Error> {
        Err(git2::Error::from_str(
            "no credentials configured for this repository",
        ))
    }
}

/// Construct a credential provider from an optional `GitHttpCredential`.
///
/// `None` produces a `PublicRepoProvider` — unauthenticated access.
/// `Some(credential)` produces a `TaskCredentialProvider`.
pub fn credential_provider_for(
    credential: Option<GitHttpCredential>,
) -> Arc<dyn GitCredentialProvider> {
    match credential {
        Some(cred) => Arc::new(TaskCredentialProvider::new(cred)),
        None => Arc::new(PublicRepoProvider),
    }
}

/* ============================
 * Event emission
 * ============================
 */

#[instrument]
pub fn send_event(
    client: ActorRef<TcpClientMessage>,
    event: GitRepositoryMessage,
    topic: String,
) -> Result<(), ActorProcessingErr> {
    let payload = to_bytes::<rancor::Error>(&event)?.to_vec();
    trace!("Forwarding event {event:?} on topic {topic}");
    let message = TcpClientMessage::Publish {
        topic,
        payload,
        trace_ctx: None,
    };
    Ok(client.send_message(message)?)
}

/* ============================
 * Supervisor
 * ============================
 */

/// Git Observer Supervisor
///
/// Enforces one worker per repo invariant.
/// Workers own exclusive access to a single git repository.
/// Git crawling is parallelized safely across repositories.
///
/// No shared mutable state. If you think you need locks here,
/// you have already made a mistake.
pub struct GitRepoSupervisor;

pub struct GitRepoSupervisorState {
    cache_root: PathBuf,
    workers: HashMap<RepoId, ActorRef<GitRepoWorkerMsg>>,
    tcp_client: ActorRef<TcpClientMessage>,
}

pub struct GitRepoSupervisorArgs {
    cache_root: PathBuf,
    tcp_client: ActorRef<TcpClientMessage>,
}

#[derive(Serialize, Deserialize, Archive, Debug)]
pub enum RepoSupervisorMessage {
    SpawnWorker {
        config: RepoObservationConfig,
    },
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

        std::fs::create_dir_all(&args.cache_root)
            .map_err(|e| ActorProcessingErr::from(e.to_string()))?;

        Ok(GitRepoSupervisorState {
            cache_root: args.cache_root,
            tcp_client: args.tcp_client,
            workers: HashMap::new(),
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            RepoSupervisorMessage::SpawnWorker { config } => {
                trace!("Received SpawnWorker message");
                let repo_id = config.repo_id.clone();

                let credential_provider = credential_provider_for(config.credentials.clone());

                let args = GitRepoWorkerArgs {
                    config,
                    credential_provider,
                    repo_path: state.cache_root.join(repo_id.to_string()),
                    tcp_client: state.tcp_client.clone(),
                };

                debug!("Starting worker for repo {}", repo_id.to_string());

                let (worker, _) = Actor::spawn_linked(
                    Some(format!("{SERVICE_NAME}.{}.worker", repo_id)),
                    GitRepoWorker,
                    args,
                    myself.clone().into(),
                )
                .await?;

                state.workers.insert(repo_id, worker);
            }
            RepoSupervisorMessage::StopWorker { .. } => {
                todo!("Implement cleanup logic for the filesystem");
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

#[derive(Serialize, Deserialize, Archive, Debug)]
pub enum GitRepoWorkerMsg {
    /// Fetch + export new data incrementally.
    Observe,
}

pub struct GitRepoWorker;

pub struct GitRepoWorkerState {
    config: RepoObservationConfig,
    tcp_client: ActorRef<TcpClientMessage>,
    repo: Repository,
    last_seen: HashMap<String, Oid>,
    credential_provider: Arc<dyn GitCredentialProvider>,
}

pub struct GitRepoWorkerArgs {
    config: RepoObservationConfig,
    repo_path: PathBuf,
    tcp_client: ActorRef<TcpClientMessage>,
    credential_provider: Arc<dyn GitCredentialProvider>,
}

fn on_commit(
    repo_id: RepoId,
    ref_name: &str,
    commit: &git2::Commit,
    tcp_client: ActorRef<TcpClientMessage>,
) -> Result<(), ActorProcessingErr> {
    debug!("Found commit for ref_name {ref_name} in repo {repo_id:?}");

    let observed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();

    let event = GitRepositoryMessage::CommitDiscovered {
        ref_name: ref_name.to_string(),
        observed_at: observed_at.to_string(),
        repo: repo_id,
        oid: commit.id().to_string(),
        committer: commit.committer().to_string(),
        time: commit.time().seconds(),
        message: commit.message().unwrap_or_default().to_string(),
        parents: commit.parent_ids().map(|id| id.to_string()).collect(),
    };

    send_event(tcp_client, event, GIT_REPO_PROCESSING_TOPIC.to_string())?;
    Ok(())
}

fn emit_ref_update(
    repo_id: RepoId,
    ref_name: &str,
    old: Option<Oid>,
    new: Oid,
    tcp_client: ActorRef<TcpClientMessage>,
) -> Result<(), ActorProcessingErr> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();

    let event = GitRepositoryMessage::RefUpdated {
        repo: repo_id,
        ref_name: ref_name.to_string(),
        old: old.map(|o| o.to_string()),
        new: new.to_string(),
        observed_at: now.to_string(),
    };

    send_event(tcp_client, event, GIT_REPO_PROCESSING_TOPIC.to_string())
}

impl GitRepoWorker {
    fn build_remote_callbacks(
        provider: Arc<dyn GitCredentialProvider>,
    ) -> RemoteCallbacks<'static> {
        let mut callbacks = RemoteCallbacks::new();
        callbacks.credentials(move |url, username_from_url, allowed| {
            provider.credentials(url, username_from_url, allowed)
        });
        callbacks
    }

    fn build_fetch_options(
        provider: Arc<dyn GitCredentialProvider>,
        depth: Option<usize>,
    ) -> FetchOptions<'static> {
        let mut fetch_opts = FetchOptions::new();
        fetch_opts.remote_callbacks(Self::build_remote_callbacks(provider));
        if let Some(d) = depth {
            fetch_opts.depth(d as i32);
        }
        fetch_opts
    }

    pub fn open_or_clone_bare(
        repo_path: &Path,
        repo_url: &str,
        credential_provider: Arc<dyn GitCredentialProvider>,
        shallow_depth: Option<usize>,
    ) -> Result<Repository, git2::Error> {
        if repo_path.exists() {
            Repository::open_bare(repo_path)
        } else {
            let mut builder = git2::build::RepoBuilder::new();
            builder.bare(true);
            let fetch_opts = Self::build_fetch_options(credential_provider, shallow_depth);
            builder.fetch_options(fetch_opts);
            builder.clone(repo_url, repo_path)
        }
    }

    fn fetch_with_optional_depth(
        repo: &Repository,
        credential_provider: Arc<dyn GitCredentialProvider>,
        remotes: &[String],
        shallow_depth: Option<usize>,
    ) -> Result<(), git2::Error> {
        for remote_name in remotes {
            let mut remote = repo.find_remote(remote_name)?;
            let mut fetch_opts =
                Self::build_fetch_options(credential_provider.clone(), shallow_depth);
            remote.fetch(&[] as &[&str], Some(&mut fetch_opts), None)?;
        }
        Ok(())
    }

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

        if let Some(stop) = last_seen {
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
            on_commit(&commit);
            if newest_seen.is_none() {
                newest_seen = Some(oid);
            }
            depth += 1;
        }

        Ok(newest_seen)
    }

    pub fn observe_repository<F>(
        repo: &Repository,
        credential_provider: &Arc<dyn GitCredentialProvider>,
        obs_config: &RepoObservationConfig,
        last_seen: &mut HashMap<String, Oid>,
        mut on_commit: F,
        mut on_ref_update: impl FnMut(&str, Option<Oid>, Oid),
    ) -> Result<(), git2::Error>
    where
        F: FnMut(&str, &git2::Commit),
    {
        Self::fetch_with_optional_depth(
            repo,
            credential_provider.clone(),
            &obs_config.remotes,
            obs_config.shallow_depth,
        )?;

        for ref_name in &obs_config.refs {
            let reference = match repo.find_reference(ref_name) {
                Ok(r) => r,
                Err(_) => continue,
            };

            let tip = match reference.target() {
                Some(t) => t,
                None => continue,
            };

            let prev = last_seen.get(ref_name).copied();

            Self::walk_commits_incremental(repo, ref_name, prev, obs_config.max_depth, |commit| {
                on_commit(ref_name, commit)
            })?;

            on_ref_update(ref_name, prev, tip);
            last_seen.insert(ref_name.to_string(), tip);
        }

        Ok(())
    }
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

        match GitRepoWorker::open_or_clone_bare(
            &args.repo_path,
            &args.config.repo_url,
            args.credential_provider.clone(),
            args.config.shallow_depth,
        ) {
            Ok(repo) => Ok(GitRepoWorkerState {
                credential_provider: args.credential_provider,
                config: args.config,
                tcp_client: args.tcp_client,
                repo,
                last_seen: HashMap::new(),
            }),
            Err(e) => {
                error!("Failed to initialize repository for observation. {e}");
                Err(e.into())
            }
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} started");
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

                GitRepoWorker::observe_repository(
                    &state.repo,
                    &state.credential_provider,
                    &state.config,
                    &mut state.last_seen,
                    |ref_name, commit| {
                        on_commit(
                            state.config.repo_id.clone(),
                            ref_name,
                            commit,
                            state.tcp_client.clone(),
                        )
                        .map_err(|e| error!("commit emission failed: {e}"))
                        .ok();
                    },
                    |ref_name, old, new| {
                        emit_ref_update(
                            state.config.repo_id.clone(),
                            ref_name,
                            old,
                            new,
                            state.tcp_client.clone(),
                        )
                        .map_err(|e| error!("ref update emission failed: {e}"))
                        .ok();
                    },
                )?;
            }
        }
        Ok(())
    }
}

/* ============================
 * Tests
 * ============================
 */

#[cfg(test)]
mod unittests {
    use super::*;
    use git2::{Commit, CredentialType, Oid, Repository};
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
        let id = RepoId::from_url("local");
        RepoObservationConfig::new(id, "local".into(), Vec::new(), Some(100), refs)
    }

    fn bare_repo() -> (TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init_bare(dir.path()).unwrap();
        (dir, repo)
    }

    fn noop_provider() -> Arc<dyn GitCredentialProvider> {
        Arc::new(PublicRepoProvider)
    }

    fn make_commit(repo: &Repository, message: &str, parents: &[&Commit]) -> Oid {
        let sig = git2::Signature::now("tester", "tester@example.com").unwrap();
        let tree_id = repo.treebuilder(None).unwrap().write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(None, &sig, &sig, message, &tree, parents)
            .unwrap()
    }

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

    // --- GitHttpCredential tests ---

    #[test]
    fn token_credential_produces_userpass() {
        let cred = GitHttpCredential::Token {
            token: "mytoken123".into(),
        };
        let (username, password) = cred.as_userpass();
        assert_eq!(username, "oauth2");
        assert_eq!(password, "mytoken123");
    }

    #[test]
    fn userpass_credential_produces_userpass() {
        let cred = GitHttpCredential::UserPass {
            username: "alice".into(),
            password: "secret".into(),
        };
        let (username, password) = cred.as_userpass();
        assert_eq!(username, "alice");
        assert_eq!(password, "secret");
    }

    #[test]
    fn task_provider_resolves_userpass_credential_type() {
        let cred = GitHttpCredential::Token {
            token: "tok".into(),
        };
        let provider = TaskCredentialProvider::new(cred);
        let result = provider.credentials(
            "https://gitlab.com/group/repo.git",
            None,
            CredentialType::USER_PASS_PLAINTEXT,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn task_provider_rejects_unsupported_credential_type() {
        let cred = GitHttpCredential::Token {
            token: "tok".into(),
        };
        let provider = TaskCredentialProvider::new(cred);
        let result = provider.credentials(
            "https://gitlab.com/group/repo.git",
            None,
            CredentialType::SSH_KEY,
        );
        assert!(result.is_err());
    }

    #[test]
    fn public_repo_provider_always_errors() {
        let provider = PublicRepoProvider;
        let result = provider.credentials(
            "https://github.com/org/public-repo.git",
            None,
            CredentialType::USER_PASS_PLAINTEXT,
        );
        assert!(result.is_err());
    }

    #[test]
    fn credential_provider_for_none_returns_public_provider() {
        let provider = credential_provider_for(None);
        let result = provider.credentials(
            "https://github.com/org/repo.git",
            None,
            CredentialType::USER_PASS_PLAINTEXT,
        );
        // PublicRepoProvider always returns an error — that's correct behavior
        assert!(result.is_err());
    }

    #[test]
    fn credential_provider_for_some_returns_task_provider() {
        let cred = GitHttpCredential::Token {
            token: "tok".into(),
        };
        let provider = credential_provider_for(Some(cred));
        let result = provider.credentials(
            "https://gitlab.com/group/repo.git",
            None,
            CredentialType::USER_PASS_PLAINTEXT,
        );
        assert!(result.is_ok());
    }

    // --- Walker / observer tests (unchanged from before) ---

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
        let mut refs_seen = HashMap::new();
        let obs_config = dummy_config(vec!["refs/heads/main".into(), "refs/heads/dev".into()]);
        let provider = noop_provider();
        GitRepoWorker::observe_repository(
            &repo,
            &provider,
            &obs_config,
            &mut last_seen,
            |r, c| seen.push((r.to_string(), c.id())),
            |ref_name, old, new| {
                refs_seen.insert(ref_name.to_string(), (old, new));
            },
        )
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
        let provider = noop_provider();
        GitRepoWorker::observe_repository(
            &repo,
            &provider,
            &config,
            &mut last_seen,
            |_r, _c| {},
            |_ref_name, _old, _new| {},
        )
        .unwrap();
        assert_eq!(last_seen["refs/heads/dev"], new_dev);
        assert_eq!(last_seen["refs/heads/main"], main_commits[0]);
    }

    #[test]
    fn empty_repo_is_noop_not_panic() {
        init_logging();
        let (_dir, repo) = bare_repo();
        let mut last_seen = HashMap::new();
        let config = dummy_config(vec!["refs/heads/main".into()]);
        let provider = noop_provider();
        let result = GitRepoWorker::observe_repository(
            &repo,
            &provider,
            &config,
            &mut last_seen,
            |_r, _c| {},
            |_ref_name, _old, _new| {},
        );
        assert!(result.is_err() || last_seen.is_empty());
    }
}
