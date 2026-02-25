use cassini_client::TcpClientMessage;
use git2::{FetchOptions, Oid, RemoteCallbacks, Repository};
use git_agent_common::{
    GitRepositoryMessage, RepoId, RepoObservationConfig, GIT_REPO_PROCESSING_TOPIC,
};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use rkyv::{rancor, to_bytes, Archive, Deserialize, Serialize};
use serde::Deserialize as SerdeDeserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, trace, warn};
use url::Url;

pub const SERVICE_NAME: &str = "polar.git.observer";
pub const REPO_SUPERVISOR_NAME: &str = "polar.git.observer.repo.supervisor";

pub mod supervisor;

use git2::{Cred, CredentialType, Error};

/// Root credential configuration.
///
/// Designed to mirror a Docker-style config layout,
/// but intentionally minimal.
#[derive(Debug, Clone, SerdeDeserialize)]
pub struct StaticCredentialConfig {
    pub hosts: HashMap<String, HostCredentialConfig>,
}

#[derive(Debug, Clone, SerdeDeserialize)]
pub struct HostCredentialConfig {
    pub http: Option<HttpCredential>,
    // TODO: Future:
    // pub ssh: Option<SshCredential>,
}

#[derive(Debug, Clone, SerdeDeserialize)]
pub struct HttpCredential {
    pub username: String,
    pub token: String,
}

/// Errors during credential configuration loading.
///
/// This keeps parsing failures separate from git2 errors.
#[derive(Debug)]
pub enum CredentialConfigError {
    Io(std::io::Error),
    Parse(serde_json::Error),
}

impl From<std::io::Error> for CredentialConfigError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for CredentialConfigError {
    fn from(e: serde_json::Error) -> Self {
        Self::Parse(e)
    }
}

impl StaticCredentialConfig {
    /// Load credential configuration from a JSON file.
    ///
    /// # Design Notes
    ///
    /// - Performs a single file read.
    /// - Fully validates JSON structure.
    /// - Returns structured error types.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, CredentialConfigError> {
        let contents = fs::read_to_string(path)?;
        let parsed = serde_json::from_str::<StaticCredentialConfig>(&contents)?;
        Ok(parsed)
    }

    pub fn from_env(var: &str) -> Result<Self, CredentialConfigError> {
        info!("Loading credentials from {var}");
        let path = std::env::var(var).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("env var missing: {var}"),
            )
        })?;
        Self::from_file(path)
    }
}

/// A simple in-memory credential provider backed by a static config.
///
/// Characteristics:
/// - Immutable after construction
/// - O(1) host lookup
/// - No runtime allocations except URL parsing
/// - Fully unit testable
pub struct StaticCredentialProvider {
    config: Arc<StaticCredentialConfig>,
}

/// A source of Git credentials.
///
/// This abstraction intentionally:
/// - Hides secret storage details
/// - Allows host-based lookup
/// - Is compatible with git2's callback model
///
/// This represents the acknowledgement of a first principle: Credentials Are Not Repo Config
/// credentials are:
/// * Cross-repo
/// * Cross-instance
/// * Environment-specific
/// * Potentially rotated
///
/// So we don’t embed credentials per repo.
/// A provider enables the workers to read from external sources operators can control
///
/// Implementations MUST:
/// - Be cheap to call (no blocking IO)
/// - Avoid heap churn in hot paths
/// - Be thread-safe
pub trait GitCredentialProvider: Send + Sync {
    /// Resolve credentials for a Git operation.
    ///
    /// Parameters are passed through from git2's credentials callback.
    ///
    /// # Arguments
    ///
    /// * `url` - Full remote URL (e.g. https://gitlab.com/group/repo.git)
    /// * `username_from_url` - Optional username parsed from URL
    /// * `allowed` - Credential types git2 is willing to accept
    ///
    /// # Behavior
    ///
    /// Implementations should:
    /// - Prefer HTTP token-based auth when available
    /// - Fall back gracefully when host not configured
    /// - Return `Error::from_str("...")` for unsupported hosts
    fn credentials(
        &self,
        url: &str,
        username_from_url: Option<&str>,
        allowed: CredentialType,
    ) -> Result<Cred, Error>;
}

impl StaticCredentialProvider {
    /// Construct from already-loaded config.
    ///
    /// Prefer loading + parsing JSON outside this type.
    pub fn new(config: StaticCredentialConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }

    /// Extract host from a remote URL.
    ///
    /// Supports:
    /// - https://gitlab.com/group/repo.git
    /// - http://...
    ///
    /// Does NOT yet support SCP-style SSH URLs.
    fn extract_host(url: &str) -> Result<String, git2::Error> {
        let parsed =
            Url::parse(url).map_err(|_| git2::Error::from_str("invalid repository URL"))?;

        parsed
            .host_str()
            .map(|h| h.to_string())
            .ok_or_else(|| git2::Error::from_str("URL missing host"))
    }
}

impl GitCredentialProvider for StaticCredentialProvider {
    fn credentials(
        &self,
        url: &str,
        _username_from_url: Option<&str>,
        allowed: CredentialType,
    ) -> Result<Cred, git2::Error> {
        // Only handle plaintext user/pass (HTTP).
        if !allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
            return Err(git2::Error::from_str("unsupported credential type"));
        }

        let host = Self::extract_host(url)?;

        let host_config = self
            .config
            .hosts
            .get(&host)
            .ok_or_else(|| git2::Error::from_str("no credentials configured for host"))?;

        let http = host_config
            .http
            .as_ref()
            .ok_or_else(|| git2::Error::from_str("no HTTP credentials configured"))?;

        Cred::userpass_plaintext(&http.username, &http.token)
    }
}

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
        trace_ctx: None,  // or WireTraceCtx::from_current_span() if you want context
    };
    Ok(client.send_message(message)?)
}

/* ============================
 * Supervisor
 * ============================
 */

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
    credential_config: StaticCredentialConfig,
}

pub struct GitRepoSupervisorArgs {
    cache_root: PathBuf,
    tcp_client: ActorRef<TcpClientMessage>,
    credential_config: StaticCredentialConfig,
}

/// Messages the supervisor accepts.
///
/// The supervisor does *not* do git work.
/// It routes, spawns, and enforces invariants.
#[derive(Serialize, Deserialize, Archive, Debug)]
pub enum RepoSupervisorMessage {
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
            credential_config: args.credential_config,
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

                let credential_provider = Arc::new(StaticCredentialProvider::new(
                    state.credential_config.clone(),
                ));

                let args = GitRepoWorkerArgs {
                    config,
                    credential_provider,
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
            RepoSupervisorMessage::StopWorker { .. } => {
                todo!("Implement cleanup logic for the filesystem");
                // trace!("Received stop message for repo {}", repo_id.to_string());
                // if let Some(worker) = state.workers.remove(&repo_id) {
                //     info!("Stopping worker for repo {}", repo_id.to_string());
                //     worker
                //         .stop_and_wait(None, Some(Duration::from_secs(300)))
                //         .await?;
                // }
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
    config: RepoObservationConfig,
    tcp_client: ActorRef<TcpClientMessage>,
    repo: Repository,
    /// Per-ref last exported commit.
    ///
    /// This MUST be persisted.
    last_seen: HashMap<String, Oid>,
    credential_provider: Arc<StaticCredentialProvider>,
}

pub struct GitRepoWorkerArgs {
    config: RepoObservationConfig,
    repo_path: PathBuf,
    tcp_client: ActorRef<TcpClientMessage>,
    credential_provider: Arc<StaticCredentialProvider>,
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
        // author: commit.author().name().unwrap_or_default().to_string(),
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
    /// Helper for constructing git2 RemoteCallbacks from a credential provider.
    ///
    /// This is intentionally isolated so:
    /// - Clone and fetch share identical auth behavior
    /// - Unit tests can inject a mock provider
    /// - No duplicated callback wiring logic exists
    fn build_remote_callbacks(provider: Arc<StaticCredentialProvider>) -> RemoteCallbacks<'static> {
        let mut callbacks = RemoteCallbacks::new();

        callbacks.credentials(move |url, username_from_url, allowed| {
            provider.credentials(url, username_from_url, allowed)
        });

        callbacks
    }

    /// Helper for constructing FetchOptions consistently.
    ///
    /// All network operations should go through this.
    fn build_fetch_options(
        provider: Arc<StaticCredentialProvider>,
        depth: Option<usize>,
    ) -> FetchOptions<'static> {
        let mut fetch_opts = FetchOptions::new();
        fetch_opts.remote_callbacks(Self::build_remote_callbacks(provider));

        if let Some(d) = depth {
            fetch_opts.depth(d as i32);
        }

        fetch_opts
    }
    /// Open an existing bare repository or clone a new one as bare.
    ///
    /// - No working tree
    /// - No checkout
    /// - No index
    ///
    /// This dramatically reduces filesystem churn and makes
    /// concurrent observation viable.
    pub fn open_or_clone_bare(
        repo_path: &Path,
        repo_url: &str,
        credential_provider: Arc<StaticCredentialProvider>,
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

    /// Fetch updates from `origin`, optionally with a shallow depth.
    ///
    /// Shallow fetches are useful for tip-only observation but
    /// explicitly trade away ancestry completeness.
    ///
    /// If `depth` is None, this performs a normal incremental fetch.
    /// If `depth` is Some(N), this performs a depth-limited fetch.
    fn fetch_with_optional_depth(
        repo: &Repository,
        credential_provider: Arc<StaticCredentialProvider>,
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
    /// Observe a repository according to an explicit policy.
    ///
    /// Observation semantics:
    /// - Commits are emitted in newest → oldest order (bounded)
    /// - Ref updates are emitted exactly once per ref per cycle
    /// - Ref updates are emitted *after* traversal completes
    ///
    /// This guarantees:
    /// - No duplicate ref → commit edges
    /// - Correct handling of force-pushes
    /// - Stable graph anchoring
    pub fn observe_repository<F>(
        repo: &Repository,
        credential_provider: &Arc<StaticCredentialProvider>,
        obs_config: &RepoObservationConfig,
        last_seen: &mut HashMap<String, Oid>,
        mut on_commit: F,
        mut on_ref_update: impl FnMut(&str, Option<Oid>, Oid),
    ) -> Result<(), git2::Error>
    where
        F: FnMut(&str, &git2::Commit),
    {
        // Always fetch first so refs represent remote truth.
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

            // Emit ref update exactly once per cycle
            on_ref_update(ref_name, prev, tip);

            // Update checkpoint only after successful traversal
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
        // Open or clone repo during actor startup.
        match Self::open_or_clone_bare(
            &args.repo_path,
            &args.config.repo_url,
            args.credential_provider.clone(),
            args.config.shallow_depth,
        ) {
            Ok(repo) =>
            // TODO: Load persisted last_seen checkpoints here.
            {
                Ok(GitRepoWorkerState {
                    credential_provider: args.credential_provider,
                    config: args.config,
                    tcp_client: args.tcp_client,
                    repo,
                    last_seen: HashMap::new(),
                })
            }
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

        // send self an Observe message on startup once we've properly cloned or opened the repo.
        myself.cast(GitRepoWorkerMsg::Observe)?;

        // TODO: Figure out how we're gonna implement the actual scheduled observation

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

                Self::observe_repository(
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

#[cfg(test)]
mod unittests {
    use super::*;
    use git2::{Commit, CredentialType, Oid, Repository};
    use std::collections::HashMap;
    use std::fs;
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
    ///Since local tests don’t fetch from remotes, we can pass a no-op credential provider.
    fn noop_provider() -> Arc<StaticCredentialProvider> {
        Arc::new(StaticCredentialProvider::new(StaticCredentialConfig {
            hosts: HashMap::new(),
        }))
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

    /// Writes a valid credential config to a temp file and returns its path.
    fn write_test_config() -> (TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");

        let json = r#"
        {
            "hosts": {
                "gitlab.com": {
                    "http": {
                        "username": "oauth2",
                        "token": "token123"
                    }
                }
            }
        }
        "#;

        fs::write(&path, json).unwrap();
        (dir, path)
    }

    #[test]
    fn loads_config_from_file() {
        let (_dir, path) = write_test_config();

        let config = StaticCredentialConfig::from_file(&path).unwrap();

        assert!(config.hosts.contains_key("gitlab.com"));
        assert_eq!(
            config.hosts["gitlab.com"].http.as_ref().unwrap().username,
            "oauth2"
        );
    }

    #[test]
    fn resolves_http_credentials_from_file_config() {
        let (_dir, path) = write_test_config();

        let config = StaticCredentialConfig::from_file(&path).unwrap();
        let provider = StaticCredentialProvider::new(config);

        let cred = provider
            .credentials(
                "https://gitlab.com/group/repo.git",
                None,
                CredentialType::USER_PASS_PLAINTEXT,
            )
            .unwrap();

        assert!(cred.has_username());
    }

    #[test]
    fn fails_for_unknown_host_from_file_config() {
        let (_dir, path) = write_test_config();

        let config = StaticCredentialConfig::from_file(&path).unwrap();
        let provider = StaticCredentialProvider::new(config);

        let result = provider.credentials(
            "https://github.com/org/repo.git",
            None,
            CredentialType::USER_PASS_PLAINTEXT,
        );

        assert!(result.is_err());
    }

    #[test]
    fn fails_on_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");

        fs::write(&path, "{ this is not valid json").unwrap();

        let result = StaticCredentialConfig::from_file(&path);
        assert!(result.is_err());
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
        let mut refs_seen = HashMap::new();

        let obs_config = dummy_config(vec!["refs/heads/main".into(), "refs/heads/dev".into()]);

        let provider = Arc::new(noop_provider());
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
        let provider = Arc::new(noop_provider());
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
