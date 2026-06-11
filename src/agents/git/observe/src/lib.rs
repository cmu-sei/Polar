use cassini_client::{OfflineBehavior, PublishRequest};
use git_agent_common::GitRepositoryMessage;
use git2::{Cred, CredentialType, FetchOptions, Oid, RemoteCallbacks, Repository};
use polar::cassini::{CassiniClient, TcpClient};
use polar::graph::nodes::git::RepoId;
use polar::topics::GIT_REPOSITORY_EVENTS;
use ractor::{Actor, ActorProcessingErr, ActorRef, SpawnErr, SupervisionEvent, async_trait};
use rkyv::{Archive, Deserialize, Serialize, rancor, to_bytes};
use serde::Deserialize as SerdeDeserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, trace, warn};
pub const SERVICE_NAME: &str = "polar.git.observer";
pub const REPO_SUPERVISOR_NAME: &str = "polar.git.observer.repo.supervisor";

pub mod supervisor;

/* ============================
 * Credentials
 * ============================
 */

/// Credentials for authenticating to a git remote over HTTPS.
///
/// # Design Notes
///
/// Token-based auth (GitHub, GitLab, Gitea, etc.) uses a single secret.
/// The username field required by HTTP Basic Auth is a protocol artifact —
/// the server does not validate it. The implementation handles this internally
/// so callers never need to know or care about the dummy username convention.
///
/// Credentials are supplied per-task by the scheduler, not loaded at agent
/// startup. `None` in `RepoObservationConfig::credentials` means the repo
/// is public and no authentication is required.
#[derive(Serialize, Deserialize, Archive, Debug, Clone)]
pub enum GitHttpCredential {
    /// Traditional username + password. Both fields are meaningful.
    UserPass { username: String, password: String },

    /// Token-based auth. The token is the only secret.
    /// The implementation supplies a conventional dummy username
    /// (e.g. "oauth2") to satisfy the HTTP Basic Auth protocol requirement.
    Token {
        token: String,
        username: Option<String>,
    },
}

impl GitHttpCredential {
    /// Resolve to a (username, password) pair suitable for git2's
    /// `Cred::userpass_plaintext`. Callers should not need this directly —
    /// use `into_git2_cred` instead.
    pub fn as_userpass(&self) -> (&str, &str) {
        match self {
            GitHttpCredential::UserPass { username, password } => (username, password),
            // "oauth2" is the conventional dummy username for token auth on
            // GitHub, GitLab, Gitea, and most other forges. The server ignores it.
            GitHttpCredential::Token { token, username } => {
                (username.as_deref().unwrap_or("oauth2"), token)
            }
        }
    }

    /// Produce a git2 credential value for use in a RemoteCallbacks handler.
    pub fn into_git2_cred(&self) -> Result<git2::Cred, git2::Error> {
        let (username, password) = self.as_userpass();
        git2::Cred::userpass_plaintext(username, password)
    }
}

/// Operator-supplied per-repo credential configuration, loaded from YAML at
/// agent startup. Keyed by normalized repo URL — see `normalize_repo_url`.
#[derive(Debug, SerdeDeserialize)]
pub struct GitAgentConfig {
    pub repos: std::collections::HashMap<String, RepoConfig>,
}

#[derive(Debug, SerdeDeserialize)]
pub struct RepoConfig {
    pub http: Option<HttpCredentialConfig>,
}

#[derive(Debug, SerdeDeserialize)]
pub struct HttpCredentialConfig {
    pub token: String,
    #[serde(default)]
    pub username: Option<String>,
}

pub enum CredentialLookup {
    Configured(GitHttpCredential),
    NotConfigured,
    /// An entry exists for this repo but contains no usable credential
    /// (empty token). This is an operator error in the config file —
    /// log loudly, but proceed as if unconfigured.
    Misconfigured,
}

impl GitAgentConfig {
    pub fn load(path: &std::path::Path) -> Result<Self, std::io::Error> {
        let raw = std::fs::read_to_string(path)?;
        let parsed: RawGitAgentConfig = serde_yaml::from_str(&raw)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let repos = parsed
            .repos
            .into_iter()
            .map(|(k, v)| (RepoId::normalize_repo_url(&k), v))
            .collect();
        Ok(GitAgentConfig { repos })
    }

    pub fn credentials_for_url(&self, normalized_url: &str) -> CredentialLookup {
        match self.repos.get(normalized_url) {
            None => CredentialLookup::NotConfigured,
            Some(RepoConfig { http: None }) => CredentialLookup::NotConfigured,
            Some(RepoConfig { http: Some(c) }) if c.token.is_empty() => {
                CredentialLookup::Misconfigured
            }
            Some(RepoConfig { http: Some(c) }) => {
                CredentialLookup::Configured(GitHttpCredential::Token {
                    token: c.token.clone(),
                    username: c.username.clone(),
                })
            }
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct RawGitAgentConfig {
    repos: std::collections::HashMap<String, RepoConfig>,
}

/* ============================
 * Shared types
 * ============================
 */

/// Observation policy applied to a repository.
///
/// This is *policy*, not state.
/// It must be supplied externally (scheduler, config, graph).
///
/// Credentials are included here so the observer never needs to load
/// them from the environment or a static file. The scheduler is responsible
/// for sourcing credentials at dispatch time. `None` means public repo —
/// attempt unauthenticated access.
#[derive(Serialize, Deserialize, Archive, Debug, Clone)]
pub struct RepoObservationConfig {
    pub repo_id: RepoId,
    pub repo_url: String,
    pub remotes: Vec<String>,
    /// Refs to observe (e.g. "refs/heads/main").
    /// If empty, caller should supply sensible defaults.
    pub refs: Vec<String>,
    /// Maximum number of commits to walk per ref.
    /// This is a hard safety bound.
    pub max_depth: usize,
    /// Optional shallow fetch depth.
    /// If None, fetch is unshallow / full (incremental).
    pub shallow_depth: Option<usize>,
    /// Frequency of observation in seconds.
    pub frequency: u64,
    /// Credentials for authenticating to the remote.
    /// None means public repo — attempt unauthenticated clone/fetch.
    pub credentials: Option<GitHttpCredential>,
}

impl RepoObservationConfig {
    /// Construct with sensible defaults for depth and frequency.
    ///
    /// Git repositories can be very large with long histories and infrequent
    /// updates. The defaults are conservative to avoid hammering remotes.
    pub fn new(
        repo_id: RepoId,
        repo_url: String,
        remotes: Vec<String>,
        shallow_depth: Option<usize>,
        refs: Vec<String>,
    ) -> Self {
        Self {
            repo_id,
            repo_url,
            remotes,
            refs,
            max_depth: 100,
            shallow_depth,
            frequency: 900,
            credentials: None,
        }
    }

    /// Construct with explicit credentials.
    pub fn new_with_credentials(
        repo_id: RepoId,
        repo_url: String,
        remotes: Vec<String>,
        shallow_depth: Option<usize>,
        refs: Vec<String>,
        credentials: GitHttpCredential,
    ) -> Self {
        Self {
            credentials: Some(credentials),
            ..Self::new(repo_id, repo_url, remotes, shallow_depth, refs)
        }
    }
}

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

#[instrument(skip(client))]
pub fn send_event(
    client: TcpClient,
    event: GitRepositoryMessage,
    topic: String,
) -> Result<(), ActorProcessingErr> {
    info!("Forwarding event to topic {topic}");
    let payload = to_bytes::<rancor::Error>(&event)?.to_vec();

    let message = PublishRequest {
        topic,
        payload,
        trace_ctx: None,
        offline_behavior: OfflineBehavior::default(),
    };

    Ok(client.publish(message)?)
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
    tcp_client: TcpClient,
}

pub struct GitRepoSupervisorArgs {
    cache_root: PathBuf,
    tcp_client: TcpClient,
}

#[derive(Serialize, Deserialize, Archive, Debug)]
pub enum RepoSupervisorMessage {
    SpawnWorker { config: RepoObservationConfig },
    StopWorker { repo_id: RepoId },
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

                match Actor::spawn_linked(
                    Some(format!("{SERVICE_NAME}.{}.worker", repo_id)),
                    GitRepoWorker,
                    args,
                    myself.into(),
                )
                .await
                {
                    Ok((worker, _)) => {
                        state.workers.insert(repo_id, worker);
                    }
                    Err(SpawnErr::ActorAlreadyRegistered(name)) => {
                        debug!("worker {name} already running, ignoring duplicate discovery event");
                        // Worth a TODO, not a blocker.
                        // One product question worth a comment-but-not-now: is "ignore the duplicate" actually the right long-term behavior,
                        // or should a rediscovery event for an already-running repo trigger the existing worker to refresh its config
                        // (e.g. if credentials changed in git.yaml and the agent got restarted/reloaded,
                        // or if max_depth/refs changed)? Right now "ignore" is correct and safe for today's test,
                        // but if discovery events are meant to be re-emitted periodically by the gitlab processor (not just once at first sight),
                        // silently ignoring them forever means config changes never propagate to already-running workers without a full agent restart.
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            RepoSupervisorMessage::StopWorker { .. } => {
                todo!("Implement cleanup logic for the filesystem");
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorFailed(cell, reason) => {
                error!("worker {cell:?} failed: {reason:?}");
                state.workers.retain(|_, w| w.get_id() != cell.get_id());
            }
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                warn!("worker {cell:?} terminated: {reason:?}");
                state.workers.retain(|_, w| w.get_id() != cell.get_id());
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
    tcp_client: TcpClient,
    repo: Repository,
    last_seen: HashMap<String, Oid>,
    credential_provider: Arc<dyn GitCredentialProvider>,
}

pub struct GitRepoWorkerArgs {
    config: RepoObservationConfig,
    repo_path: PathBuf,
    tcp_client: TcpClient,
    credential_provider: Arc<dyn GitCredentialProvider>,
}

fn on_commit(
    repo_id: RepoId,
    ref_name: &str,
    commit: &git2::Commit,
    tcp_client: TcpClient,
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

    send_event(tcp_client, event, GIT_REPOSITORY_EVENTS.to_string())?;
    Ok(())
}

fn emit_ref_update(
    repo_id: RepoId,
    ref_name: &str,
    old: Option<Oid>,
    new: Oid,
    tcp_client: TcpClient,
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

    send_event(tcp_client, event, GIT_REPOSITORY_EVENTS.to_string())
}

impl GitRepoWorker {
    fn resolve_default_refs(repo: &Repository) -> Vec<String> {
        match repo.find_reference("refs/remotes/origin/HEAD") {
            Ok(head_ref) => match head_ref.symbolic_target() {
                Some(target) => {
                    debug!("resolved default ref to {target}");
                    vec![target.to_string()]
                }
                None => {
                    warn!(
                        "refs/remotes/origin/HEAD has no symbolic target, falling back to refs/remotes/origin/main"
                    );
                    vec!["refs/remotes/origin/main".to_string()]
                }
            },
            Err(e) => {
                warn!(
                    "could not read refs/remotes/origin/HEAD ({e}), falling back to refs/remotes/origin/main"
                );
                vec!["refs/remotes/origin/main".to_string()]
            }
        }
    }

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
                Err(e) => {
                    warn!("ref {ref_name} not found ({e}), skipping");
                    continue;
                }
            };
            let tip = match reference.target() {
                Some(t) => t,
                None => {
                    warn!("ref {ref_name} has no direct target, skipping");
                    continue;
                }
            };
            debug!(
                "observing {ref_name}: tip={tip}, last_seen={:?}",
                last_seen.get(ref_name)
            );

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
            Ok(repo) => {
                let mut config = args.config;
                if config.refs.is_empty() {
                    config.refs = Self::resolve_default_refs(&repo);
                    info!(
                        "no refs configured for {:?}, defaulting to {:?}",
                        config.repo_id, config.refs
                    );
                }
                Ok(GitRepoWorkerState {
                    credential_provider: args.credential_provider,
                    config,
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
        myself.cast(GitRepoWorkerMsg::Observe)?;
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
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

                myself.send_after(
                    std::time::Duration::from_secs(state.config.frequency),
                    || GitRepoWorkerMsg::Observe,
                );
            }
        }
        Ok(())
    }
}
