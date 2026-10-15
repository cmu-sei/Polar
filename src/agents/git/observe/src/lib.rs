#![deny(clippy::disallowed_methods)]

use crate::status::{Outcome, StatusTracker};
use git_agent_common::GitRepositoryMessage;
use git_agent_common::RepoObservationConfig;
use git_agent_common::RepositorySet;
use git_agent_common::status::{GIT_REPO_OBSERVATION_STATUS, ObservationStatus};
use git2::{Cred, CredentialType, FetchOptions, Oid, RemoteCallbacks, Repository};
use polar::cassini::TcpClient;
use polar::graph::nodes::git::RepoId;
use polar::topics::GIT_REPOSITORY_EVENTS;
use polar_agent_builder::CryptoClient;
use ractor::{Actor, ActorProcessingErr, ActorRef, SpawnErr, SupervisionEvent, async_trait};
use rkyv::{Archive, Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, trace, warn};

pub const SERVICE_NAME: &str = "polar.git.observer";
pub const REPO_SUPERVISOR_NAME: &str = "polar.git.observer.repo.supervisor";

/// How long a worker that is being replaced is given to stop: it may be in the middle of a fetch.
const WORKER_GRACE: ractor::concurrency::Duration = ractor::concurrency::Duration::from_secs(30);

pub mod agent;
pub mod credentials;
pub mod settings;
pub mod status;
pub mod supervisor;

fn git_time_to_epoch_ms(t: &git2::Time) -> i64 {
    t.seconds() * 1000
}

/// The refs to read in the clone for the refs an author asked for. A bare clone's `refs/heads/main` stops
/// moving after the clone; a fetch advances only `refs/remotes/<remote>/main`. So a configured `refs/heads/X`
/// is read from `refs/remotes/<remote>/X`, the remote being the first one fetched (`origin` when none is
/// named). Anything else (`refs/remotes/...`, `refs/tags/...`, a name that is not a full ref) is taken as
/// written, and an empty list stays empty, for the default branch to be found after the clone.
pub fn tracked_refs(remotes: &[String], refs: &[String]) -> Vec<String> {
    let remote = remotes.first().map(String::as_str).unwrap_or("origin");
    refs.iter()
        .map(|r| match r.strip_prefix("refs/heads/") {
            Some(branch) => format!("refs/remotes/{remote}/{branch}"),
            None => r.clone(),
        })
        .collect()
}

/* ============================
 * Shared types
 * ============================
 */

/* ============================
 * Credential Provider
 * ============================
 */

/// How a worker makes its clone: `GitRepoWorker::open_or_clone_bare` outside a test. A function pointer so that a
/// test can put another in its place and need no network, and so that it can see what the clone is given.
pub type CloneFn = fn(
    &Path,
    &str,
    Arc<dyn GitCredentialProvider>,
    Option<usize>,
) -> Result<Repository, git2::Error>;

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

/// A no-op credential provider for public repositories.
///
/// Used when `RepoObservationConfig::credential` is `None`.
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
    git_agent_common::publish_event(&client, &topic, &event).map_err(ActorProcessingErr::from)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// Says what became of an attempt. Failing to say so is logged and never fails the attempt: the status is for
/// people, and the observation is the work.
fn publish_status(client: &TcpClient, status: &ObservationStatus) {
    if let Err(e) = git_agent_common::publish_event(client, GIT_REPO_OBSERVATION_STATUS, status) {
        warn!("could not publish the status of {}: {e}", status.repo_id);
    }
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

/// A worker that is running, and the configuration it was started with, as announced.
struct RunningWorker {
    actor: ActorRef<GitRepoWorkerMsg>,
    config: RepoObservationConfig,
}

pub struct GitRepoSupervisorState {
    cache_root: PathBuf,
    workers: HashMap<RepoId, RunningWorker>,
    tcp_client: TcpClient,
    crypto: Arc<dyn CryptoClient>,
    clone: CloneFn,
}

pub struct GitRepoSupervisorArgs {
    cache_root: PathBuf,
    tcp_client: TcpClient,
    /// Where each worker asks for the credential its repository names.
    crypto: Arc<dyn CryptoClient>,
    clone: CloneFn,
}

#[derive(Serialize, Deserialize, Archive, Debug)]
pub enum RepoSupervisorMessage {
    SpawnWorker {
        config: RepoObservationConfig,
    },
    StopWorker {
        repo_id: RepoId,
    },
    /// The `credentials` section was replaced: a repository that was waiting for its credential tries again.
    CredentialsChanged,
    /// Everything the controller wants observed. Each repository is started, or replaced if its settings
    /// changed, or left alone if it is as announced; and if the set is `complete`, the workers of repositories
    /// it does not name are stopped.
    Reconcile {
        set: RepositorySet,
    },
}

/// What a failed attempt to start a repository's worker means for the supervisor of all of them.
///
/// A worker already running (the repository was announced twice) is expected. A worker that cannot
/// start (an unreachable host, bad credentials) is that repository's problem and is logged: it
/// used to fail the supervisor, which ended the whole agent. Nothing starts it again until the
/// repository is announced again.
pub fn tolerate_spawn_failure(repo_id: &RepoId, error: SpawnErr) -> Result<(), ActorProcessingErr> {
    match error {
        SpawnErr::ActorAlreadyRegistered(name) => {
            debug!("worker {name} already running, ignoring duplicate discovery event");
            Ok(())
        }
        SpawnErr::StartupFailed(reason) => {
            error!("the worker for {repo_id} could not start, so it is not observed: {reason}");
            Ok(())
        }
        other => Err(other.into()),
    }
}

impl GitRepoSupervisor {
    /// Makes sure a worker is running for `config`: starts one, or replaces the one running if the settings
    /// changed, or leaves it alone if it is as announced.
    async fn ensure_worker(
        myself: &ActorRef<RepoSupervisorMessage>,
        state: &mut GitRepoSupervisorState,
        config: RepoObservationConfig,
    ) -> Result<(), ActorProcessingErr> {
        trace!("Received SpawnWorker message");
        let repo_id = config.repo_id.clone();

        // The same announcement again is nothing to do. A changed one replaces the worker: the
        // clone in the cache is reused, so only the settings change.
        match state.workers.get(&repo_id) {
            Some(running) if running.config == config => {
                debug!("worker for {repo_id} already running with this configuration");
                return Ok(());
            }
            Some(_) => {
                info!("the configuration of {repo_id} changed; restarting its worker");
                if let Some(old) = state.workers.remove(&repo_id)
                    && let Err(e) = old
                        .actor
                        .stop_and_wait(Some("configuration changed".into()), Some(WORKER_GRACE))
                        .await
                {
                    warn!("the old worker for {repo_id} did not stop in time: {e:?}");
                }
            }
            None => {}
        }

        let announced = config.clone();
        let args = GitRepoWorkerArgs {
            config,
            crypto: state.crypto.clone(),
            clone: state.clone,
            repo_path: state.cache_root.join(repo_id.to_string()),
            tcp_client: state.tcp_client.clone(),
        };

        match Actor::spawn_linked(
            Some(format!("{SERVICE_NAME}.{}.worker", repo_id)),
            GitRepoWorker,
            args,
            myself.clone().into(),
        )
        .await
        {
            Ok((actor, _)) => {
                state.workers.insert(
                    repo_id,
                    RunningWorker {
                        actor,
                        config: announced,
                    },
                );
            }
            Err(e) => tolerate_spawn_failure(&repo_id, e)?,
        }
        Ok(())
    }
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
            crypto: args.crypto,
            clone: args.clone,
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
                Self::ensure_worker(&myself, state, config).await?;
            }
            RepoSupervisorMessage::Reconcile { set } => {
                let named: std::collections::HashSet<RepoId> =
                    set.repositories.iter().map(|c| c.repo_id.clone()).collect();
                for config in set.repositories {
                    Self::ensure_worker(&myself, state, config).await?;
                }
                if set.complete {
                    let gone: Vec<RepoId> = state
                        .workers
                        .keys()
                        .filter(|id| !named.contains(*id))
                        .cloned()
                        .collect();
                    for repo_id in gone {
                        // The clone stays in the cache: it is reused if the repository is named again.
                        if let Some(worker) = state.workers.remove(&repo_id) {
                            info!("{repo_id} is no longer in the set; stopping its worker");
                            worker.actor.stop(Some("no longer in the set".into()));
                        }
                    }
                }
            }
            RepoSupervisorMessage::CredentialsChanged => {
                for worker in state.workers.values() {
                    if let Err(e) = worker.actor.cast(GitRepoWorkerMsg::CredentialsChanged) {
                        warn!("could not tell a worker the credentials changed: {e}");
                    }
                }
            }
            RepoSupervisorMessage::StopWorker { repo_id } => {
                // The clone stays in the cache: it is reused if the repository is observed again.
                match state.workers.remove(&repo_id) {
                    Some(worker) => worker.actor.stop(Some("asked to stop".into())),
                    None => warn!("asked to stop the worker for {repo_id}, which is not running"),
                }
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
                state
                    .workers
                    .retain(|_, w| w.actor.get_id() != cell.get_id());
            }
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                warn!("worker {cell:?} terminated: {reason:?}");
                state
                    .workers
                    .retain(|_, w| w.actor.get_id() != cell.get_id());
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
    /// The `credentials` section was replaced. A worker waiting for its credential tries at once; any other
    /// ignores it, since it asks for its credential again at every fetch.
    CredentialsChanged,
}

pub struct GitRepoWorker;

pub struct GitRepoWorkerState {
    config: RepoObservationConfig,
    repo_path: PathBuf,
    tcp_client: TcpClient,
    crypto: Arc<dyn CryptoClient>,
    clone: CloneFn,
    /// Cloned or opened by the first attempt that has what it needs, not before: a repository whose
    /// credential has not arrived has no clone yet.
    repo: Option<Repository>,
    last_seen: HashMap<String, Oid>,
    /// The last attempt could not go ahead (its credential is missing or unusable, or the clone or the fetch
    /// failed).
    waiting: bool,
    /// What the attempts so far amount to, for the status published after each.
    status: StatusTracker,
    /// The next scheduled attempt, replaced whenever one runs so there is never more than one.
    next: Option<tokio::task::JoinHandle<Result<(), ractor::MessagingErr<GitRepoWorkerMsg>>>>,
}

pub struct GitRepoWorkerArgs {
    config: RepoObservationConfig,
    repo_path: PathBuf,
    tcp_client: TcpClient,
    crypto: Arc<dyn CryptoClient>,
    clone: CloneFn,
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
        time: git_time_to_epoch_ms(&commit.time()), // convert to ms to keep in line with other timestamps
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
    /// One attempt to observe the repository: ask for the credential it names, clone it if that has not
    /// been done, then fetch and publish. An attempt that cannot go ahead because the credential is
    /// missing or unusable, or the clone or the fetch failed, is logged and leaves the worker waiting for the
    /// next (the next fetch time, or the credentials changing); it does not end the worker. However it ends,
    /// the status is published, so a person can see which it was without reading this pod's log.
    async fn attempt(&self, state: &mut GitRepoWorkerState) {
        let outcome = self.try_to_observe(state).await;
        let status = state.status.record(outcome, now_ms());
        publish_status(&state.tcp_client, &status);
    }

    async fn try_to_observe(&self, state: &mut GitRepoWorkerState) -> Outcome {
        let provider = match credentials::resolve(&*state.crypto, &state.config).await {
            credentials::Resolution::Ready(provider) => provider,
            credentials::Resolution::Pending => {
                info!(
                    "{} is waiting for the credential it names",
                    state.config.repo_id
                );
                state.waiting = true;
                return Outcome::PendingCredential;
            }
            credentials::Resolution::Failed(why) => {
                warn!("{} is not observed: {why}", state.config.repo_id);
                state.waiting = true;
                return Outcome::CredentialUnusable(why);
            }
        };

        if state.repo.is_none() {
            match (state.clone)(
                &state.repo_path,
                &state.config.repo_url,
                provider.clone(),
                state.config.shallow_depth,
            ) {
                Ok(repo) => {
                    if state.config.refs.is_empty() {
                        state.config.refs = Self::resolve_default_refs(&repo);
                        info!(
                            "no refs configured for {:?}, defaulting to {:?}",
                            state.config.repo_id, state.config.refs
                        );
                    }
                    state.repo = Some(repo);
                }
                Err(e) => {
                    error!(
                        "{} could not be cloned, so it is not observed yet: {e}",
                        state.config.repo_id
                    );
                    state.waiting = true;
                    return Outcome::CloneFailed(e.message().to_string());
                }
            }
        }

        let Some(repo) = state.repo.as_ref() else {
            return Outcome::CloneFailed("no clone".into());
        };
        let observed = Self::observe_repository(
            repo,
            &provider,
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
        );
        match observed {
            Ok(()) => {
                state.waiting = false;
                Outcome::Observed {
                    tips: state
                        .last_seen
                        .iter()
                        .map(|(name, oid)| (name.clone(), oid.to_string()))
                        .collect(),
                }
            }
            Err(e) => {
                // The worker stays: a remote that is down for a while is the ordinary case, and the next
                // attempt fetches again from where this one stopped.
                error!(
                    "{} could not be fetched, so it is not up to date: {e}",
                    state.config.repo_id
                );
                state.waiting = true;
                Outcome::FetchFailed(e.message().to_string())
            }
        }
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
        // Nothing is cloned here: the repository may name a credential that has not arrived, and the first
        // attempt (post_start) is the one that finds out.
        let mut config = args.config;
        config.refs = tracked_refs(&config.remotes, &config.refs);
        Ok(GitRepoWorkerState {
            repo_path: args.repo_path,
            tcp_client: args.tcp_client,
            crypto: args.crypto,
            clone: args.clone,
            repo: None,
            last_seen: HashMap::new(),
            waiting: false,
            status: StatusTracker::new(config.repo_id.clone()),
            config,
            next: None,
        })
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
                if let Some(earlier) = state.next.take() {
                    earlier.abort();
                }

                self.attempt(state).await;

                state.next = Some(myself.send_after(
                    std::time::Duration::from_secs(state.config.frequency),
                    || GitRepoWorkerMsg::Observe,
                ));
            }
            GitRepoWorkerMsg::CredentialsChanged => {
                if state.waiting {
                    debug!(
                        "the credentials changed; {} tries again",
                        state.config.repo_id
                    );
                    myself.cast(GitRepoWorkerMsg::Observe)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::test_support::{Answer, Recorder};
    use cassini_client::TcpClientMessage;
    use git_agent_common::status::ObservationState;
    use git2::CredentialType;
    use std::sync::Mutex;

    /// Takes whatever the worker would publish, and says nothing.
    struct Sink {
        received: Arc<Mutex<Vec<TcpClientMessage>>>,
    }

    #[async_trait]
    impl Actor for Sink {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _: ActorRef<Self::Msg>,
            _: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            message: Self::Msg,
            _: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            self.received.lock().unwrap().push(message);
            Ok(())
        }
    }

    struct Rig {
        worker: ActorRef<GitRepoWorkerMsg>,
        crypto: Arc<Recorder>,
        received: Arc<Mutex<Vec<TcpClientMessage>>>,
        cache: tempfile::TempDir,
    }

    /// A clone that makes no connection: it records what it was given and fails, as an unreachable host would.
    fn refusing_clone(
        _path: &Path,
        url: &str,
        provider: Arc<dyn GitCredentialProvider>,
        _depth: Option<usize>,
    ) -> Result<Repository, git2::Error> {
        let credential_offered = provider
            .credentials(url, None, CredentialType::USER_PASS_PLAINTEXT)
            .is_ok();
        CLONES
            .lock()
            .unwrap()
            .push((url.to_string(), credential_offered));
        Err(git2::Error::from_str("test: there is no network"))
    }

    /// What `refusing_clone` was asked: the URL, and whether the provider it was given offers a credential.
    static CLONES: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());

    fn clones_of(url: &str) -> Vec<bool> {
        CLONES
            .lock()
            .unwrap()
            .iter()
            .filter(|(u, _)| u == url)
            .map(|(_, offered)| *offered)
            .collect()
    }

    async fn start(config: RepoObservationConfig) -> Rig {
        start_with(config, GitRepoWorker::open_or_clone_bare).await
    }

    async fn start_with(config: RepoObservationConfig, clone: CloneFn) -> Rig {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (sink, _) = Actor::spawn(
            None,
            Sink {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();
        let crypto = Arc::new(Recorder::default());
        let cache = tempfile::tempdir().unwrap();
        let (worker, _) = Actor::spawn(
            None,
            GitRepoWorker,
            GitRepoWorkerArgs {
                repo_path: cache.path().join("clone"),
                config,
                tcp_client: TcpClient::from_actor_ref(sink),
                crypto: crypto.clone(),
                clone,
            },
        )
        .await
        .unwrap();
        Rig {
            worker,
            crypto,
            received,
            cache,
        }
    }

    #[test]
    fn a_branch_is_read_from_the_remote_tracking_ref_a_fetch_advances() {
        let origin = vec!["origin".to_string()];
        let refs = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            tracked_refs(&origin, &refs(&["refs/heads/main", "refs/heads/feature/x"])),
            refs(&["refs/remotes/origin/main", "refs/remotes/origin/feature/x"])
        );
        // the first remote is the one fetched; none named is origin
        assert_eq!(
            tracked_refs(
                &["upstream".to_string(), "origin".to_string()],
                &refs(&["refs/heads/dev"])
            ),
            refs(&["refs/remotes/upstream/dev"])
        );
        assert_eq!(
            tracked_refs(&[], &refs(&["refs/heads/dev"])),
            refs(&["refs/remotes/origin/dev"])
        );
        // anything else is as written, and nothing stays nothing
        assert_eq!(
            tracked_refs(
                &origin,
                &refs(&["refs/remotes/origin/main", "refs/tags/v1", "HEAD", "main"])
            ),
            refs(&["refs/remotes/origin/main", "refs/tags/v1", "HEAD", "main"])
        );
        assert!(tracked_refs(&origin, &[]).is_empty());
    }

    #[tokio::test]
    async fn a_worker_reads_the_refs_a_fetch_advances_not_the_ones_the_clone_froze() {
        let origin = tempfile::tempdir().unwrap();
        let repo = Repository::init_bare(origin.path()).unwrap();
        let sig = git2::Signature::now("t", "t@example.org").unwrap();
        let tree = repo
            .find_tree(repo.treebuilder(None).unwrap().write().unwrap())
            .unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "first", &tree, &[])
            .unwrap();
        repo.set_head("refs/heads/main").unwrap();
        let url = origin.path().to_string_lossy().to_string();
        let mut config = RepoObservationConfig::new(
            RepoId::from_url(&url),
            url,
            vec!["origin".into()],
            None,
            vec!["refs/heads/main".into()],
        );
        config.frequency = 1;
        let rig = start(config).await;
        until("the first commit", || {
            published_commits(&rig.received).contains(&"first".to_string())
        })
        .await;
        // A second commit arrives at the origin; only a fetch into refs/remotes can show it.
        let first = repo
            .find_reference("refs/heads/main")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        repo.commit(
            Some("refs/heads/main"),
            &sig,
            &sig,
            "second",
            &tree,
            &[&first],
        )
        .unwrap();
        until("the second commit", || {
            published_commits(&rig.received).contains(&"second".to_string())
        })
        .await;
        rig.worker.stop(None);
    }

    /// The messages of the commits the worker has published so far.
    fn published_commits(received: &Mutex<Vec<TcpClientMessage>>) -> Vec<String> {
        received
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                TcpClientMessage::Publish { payload, .. } => {
                    let mut aligned = rkyv::util::AlignedVec::<16>::new();
                    aligned.extend_from_slice(payload);
                    match rkyv::from_bytes::<GitRepositoryMessage, rkyv::rancor::Error>(&aligned) {
                        Ok(GitRepositoryMessage::CommitDiscovered { message, .. }) => Some(message),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect()
    }

    /// The statuses the worker has published so far, oldest first.
    fn published_statuses(received: &Mutex<Vec<TcpClientMessage>>) -> Vec<ObservationStatus> {
        received
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                TcpClientMessage::Publish { topic, payload, .. }
                    if topic == GIT_REPO_OBSERVATION_STATUS =>
                {
                    let mut aligned = rkyv::util::AlignedVec::<16>::new();
                    aligned.extend_from_slice(payload);
                    rkyv::from_bytes::<ObservationStatus, rkyv::rancor::Error>(&aligned).ok()
                }
                _ => None,
            })
            .collect()
    }

    fn last_status(rig: &Rig) -> Option<ObservationStatus> {
        published_statuses(&rig.received).pop()
    }

    /// A repository with one commit on `main`, as a bare origin on disk, and the configuration that observes it.
    fn local_origin(frequency: u64) -> (tempfile::TempDir, Repository, RepoObservationConfig) {
        let origin = tempfile::tempdir().unwrap();
        let repo = Repository::init_bare(origin.path()).unwrap();
        {
            let sig = git2::Signature::now("t", "t@example.org").unwrap();
            let tree = repo
                .find_tree(repo.treebuilder(None).unwrap().write().unwrap())
                .unwrap();
            repo.commit(Some("refs/heads/main"), &sig, &sig, "first", &tree, &[])
                .unwrap();
        }
        repo.set_head("refs/heads/main").unwrap();
        let url = origin.path().to_string_lossy().to_string();
        let mut config = RepoObservationConfig::new(
            RepoId::from_url(&url),
            url,
            vec!["origin".into()],
            None,
            vec!["refs/heads/main".into()],
        );
        config.frequency = frequency;
        (origin, repo, config)
    }

    #[tokio::test]
    async fn a_repository_waiting_for_its_credential_says_so() {
        let rig = start(private_repo()).await;
        until("a status", || last_status(&rig).is_some()).await;
        settle().await;
        let statuses = published_statuses(&rig.received);
        assert_eq!(statuses.len(), 1, "one attempt, one status");
        assert_eq!(statuses[0].state, ObservationState::PendingCredential);
        assert_eq!(statuses[0].repo_id, private_repo().repo_id);
        assert_eq!(statuses[0].reason, None);
        assert_eq!(statuses[0].consecutive_failures, 0);
        rig.worker.stop(None);
    }

    #[tokio::test]
    async fn a_credential_bound_to_another_host_is_a_failure_that_says_why() {
        let rig = start(private_repo()).await;
        *rig.crypto.answer.lock().unwrap() = Answer::HostNotBound;
        until("the first attempt", || asked(&rig) >= 1).await;
        until("a status", || last_status(&rig).is_some()).await;
        let status = last_status(&rig).unwrap();
        assert_eq!(status.state, ObservationState::Failing);
        assert!(
            status
                .reason
                .as_deref()
                .unwrap()
                .starts_with("the credential cannot be used:"),
            "{:?}",
            status.reason
        );
        assert_eq!(status.consecutive_failures, 1);
        rig.worker.stop(None);
    }

    #[tokio::test]
    async fn a_clone_that_fails_is_a_failure_that_counts_and_the_worker_goes_on() {
        let url = "https://localhost:1/status/r.git";
        let rig = start_with(private_repo_at(url), refusing_clone).await;
        *rig.crypto.answer.lock().unwrap() = Answer::Found {
            username: "svc",
            secret: b"tok",
        };
        rig.worker
            .cast(GitRepoWorkerMsg::CredentialsChanged)
            .unwrap();
        until("a failed clone", || {
            last_status(&rig).is_some_and(|s| s.state == ObservationState::Failing)
        })
        .await;
        let first = last_status(&rig).unwrap();
        assert_eq!(
            first.reason.as_deref(),
            Some("the clone failed: test: there is no network")
        );
        rig.worker.cast(GitRepoWorkerMsg::Observe).unwrap();
        until("a second failure", || {
            last_status(&rig).is_some_and(|s| s.consecutive_failures == 2)
        })
        .await;
        rig.worker.stop(None);
    }

    #[tokio::test]
    async fn a_repository_that_is_fetched_is_observing_with_its_tips_and_the_time() {
        let (_origin, repo, config) = local_origin(900);
        let tip = repo.refname_to_id("refs/heads/main").unwrap().to_string();
        let before = now_ms();
        let rig = start(config).await;
        until("observing", || {
            last_status(&rig).is_some_and(|s| s.state == ObservationState::Observing)
        })
        .await;
        let status = last_status(&rig).unwrap();
        assert_eq!(status.reason, None);
        assert_eq!(status.consecutive_failures, 0);
        assert!(status.last_success.unwrap() >= before);
        assert_eq!(
            status.tips,
            vec![("refs/remotes/origin/main".to_string(), tip)]
        );
        rig.worker.stop(None);
    }

    #[tokio::test]
    async fn a_fetch_that_fails_does_not_end_the_worker_and_the_next_attempt_recovers() {
        let (origin, repo, config) = local_origin(900);
        let rig = start(config).await;
        until("observing", || {
            last_status(&rig).is_some_and(|s| s.state == ObservationState::Observing)
        })
        .await;
        let observed_at = last_status(&rig).unwrap().last_success;

        // The origin goes away: the next fetch cannot work.
        let gone = origin.path().with_extension("gone");
        std::fs::rename(origin.path(), &gone).unwrap();
        rig.worker.cast(GitRepoWorkerMsg::Observe).unwrap();
        until("a failed fetch", || {
            last_status(&rig).is_some_and(|s| s.state == ObservationState::Failing)
        })
        .await;
        let failing = last_status(&rig).unwrap();
        assert!(
            failing
                .reason
                .as_deref()
                .unwrap()
                .starts_with("the fetch failed:"),
            "{:?}",
            failing.reason
        );
        assert_eq!(failing.consecutive_failures, 1);
        assert_eq!(
            failing.last_success, observed_at,
            "the last success is still the last success"
        );
        assert!(
            !failing.tips.is_empty(),
            "where the refs were is still known"
        );
        assert!(
            rig.worker.get_status() != ractor::ActorStatus::Stopped
                && rig.worker.get_status() != ractor::ActorStatus::Stopping,
            "a failed fetch ended the worker"
        );

        // It comes back, with a new commit; the same worker sees it.
        std::fs::rename(&gone, origin.path()).unwrap();
        let sig = git2::Signature::now("t", "t@example.org").unwrap();
        let tree = repo
            .find_tree(repo.treebuilder(None).unwrap().write().unwrap())
            .unwrap();
        let first = repo
            .find_reference("refs/heads/main")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        repo.commit(
            Some("refs/heads/main"),
            &sig,
            &sig,
            "second",
            &tree,
            &[&first],
        )
        .unwrap();
        rig.worker.cast(GitRepoWorkerMsg::Observe).unwrap();
        until("observing again", || {
            last_status(&rig).is_some_and(|s| s.state == ObservationState::Observing)
        })
        .await;
        let recovered = last_status(&rig).unwrap();
        assert_eq!(recovered.reason, None);
        assert_eq!(recovered.consecutive_failures, 0);
        assert!(published_commits(&rig.received).contains(&"second".to_string()));
        rig.worker.stop(None);
    }

    #[tokio::test]
    async fn a_credentials_change_hurries_a_failing_fetch_but_not_a_healthy_repository() {
        let (origin, _repo, config) = local_origin(900);
        let rig = start(config).await;
        until("observing", || {
            last_status(&rig).is_some_and(|s| s.state == ObservationState::Observing)
        })
        .await;
        settle().await;
        let healthy = published_statuses(&rig.received).len();
        rig.worker
            .cast(GitRepoWorkerMsg::CredentialsChanged)
            .unwrap();
        settle().await;
        assert_eq!(
            published_statuses(&rig.received).len(),
            healthy,
            "a repository that is being observed has no reason to try again because credentials changed"
        );

        let gone = origin.path().with_extension("gone");
        std::fs::rename(origin.path(), &gone).unwrap();
        rig.worker.cast(GitRepoWorkerMsg::Observe).unwrap();
        until("a failed fetch", || {
            last_status(&rig).is_some_and(|s| s.state == ObservationState::Failing)
        })
        .await;
        let failing = published_statuses(&rig.received).len();
        // Its frequency is 900 s: only the message can make the next attempt now.
        rig.worker
            .cast(GitRepoWorkerMsg::CredentialsChanged)
            .unwrap();
        until("an attempt made because the credentials changed", || {
            published_statuses(&rig.received).len() > failing
        })
        .await;

        // Once it has recovered it is healthy again, and a change of credentials is nothing to it.
        std::fs::rename(&gone, origin.path()).unwrap();
        rig.worker.cast(GitRepoWorkerMsg::Observe).unwrap();
        until("observing again", || {
            last_status(&rig).is_some_and(|s| s.state == ObservationState::Observing)
        })
        .await;
        settle().await;
        let recovered = published_statuses(&rig.received).len();
        rig.worker
            .cast(GitRepoWorkerMsg::CredentialsChanged)
            .unwrap();
        settle().await;
        assert_eq!(published_statuses(&rig.received).len(), recovered);
        rig.worker.stop(None);
    }

    struct SupervisorRig {
        supervisor: ActorRef<RepoSupervisorMessage>,
        crypto: Arc<Recorder>,
        _cache: tempfile::TempDir,
    }

    async fn start_supervisor() -> SupervisorRig {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (sink, _) = Actor::spawn(None, Sink { received }, ()).await.unwrap();
        let crypto = Arc::new(Recorder::default());
        let cache = tempfile::tempdir().unwrap();
        let (supervisor, _) = Actor::spawn(
            None,
            GitRepoSupervisor,
            GitRepoSupervisorArgs {
                cache_root: cache.path().to_path_buf(),
                tcp_client: TcpClient::from_actor_ref(sink),
                crypto: crypto.clone(),
                clone: refusing_clone,
            },
        )
        .await
        .unwrap();
        SupervisorRig {
            supervisor,
            crypto,
            _cache: cache,
        }
    }

    fn private_repo_at(url: &str) -> RepoObservationConfig {
        RepoObservationConfig::new(
            RepoId::from_url(url),
            url.into(),
            vec!["origin".into()],
            None,
            Vec::new(),
        )
        .with_credential("gh")
    }

    #[tokio::test]
    async fn the_same_announcement_again_changes_nothing_and_a_changed_one_replaces_the_worker() {
        let rig = start_supervisor().await;
        let asked = || rig.crypto.asked.lock().unwrap().len();
        let config = private_repo_at("https://localhost:1/restart/r.git");
        rig.supervisor
            .cast(RepoSupervisorMessage::SpawnWorker {
                config: config.clone(),
            })
            .unwrap();
        until("the first attempt", || asked() == 1).await;

        // Announced again, unchanged: no new worker, so no new attempt.
        rig.supervisor
            .cast(RepoSupervisorMessage::SpawnWorker {
                config: config.clone(),
            })
            .unwrap();
        settle().await;
        assert_eq!(
            asked(),
            1,
            "an identical announcement started another attempt"
        );

        // Announced with a setting changed: the old worker is stopped and a new one starts.
        let mut changed = config.clone();
        changed.frequency = 901;
        rig.supervisor
            .cast(RepoSupervisorMessage::SpawnWorker { config: changed })
            .unwrap();
        until("the new worker's first attempt", || asked() == 2).await;

        // Only the new worker is alive: telling the workers makes exactly one more attempt.
        rig.supervisor
            .cast(RepoSupervisorMessage::CredentialsChanged)
            .unwrap();
        until("one more attempt", || asked() == 3).await;
        settle().await;
        assert_eq!(asked(), 3, "the replaced worker is still running");
        rig.supervisor.stop(None);
    }

    #[tokio::test]
    async fn repositories_are_restarted_one_at_a_time_each_by_its_own_change() {
        let rig = start_supervisor().await;
        let asked = || rig.crypto.asked.lock().unwrap().len();
        let (a, b) = (
            private_repo_at("https://localhost:1/two/a.git"),
            private_repo_at("https://localhost:1/two/b.git"),
        );
        for config in [a.clone(), b.clone()] {
            rig.supervisor
                .cast(RepoSupervisorMessage::SpawnWorker { config })
                .unwrap();
        }
        until("both first attempts", || asked() == 2).await;
        let mut changed = a.clone();
        changed.max_depth = 7;
        rig.supervisor
            .cast(RepoSupervisorMessage::SpawnWorker { config: changed })
            .unwrap();
        until("a's new worker", || asked() == 3).await;
        rig.supervisor
            .cast(RepoSupervisorMessage::CredentialsChanged)
            .unwrap();
        // a's new worker and b's untouched one: two attempts, not three.
        until("both asked again", || asked() == 5).await;
        settle().await;
        assert_eq!(asked(), 5);
        rig.supervisor.stop(None);
    }

    fn asked(rig: &Rig) -> usize {
        rig.crypto.asked.lock().unwrap().len()
    }

    async fn until(what: &str, mut condition: impl FnMut() -> bool) {
        for _ in 0..600 {
            if condition() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for {what}");
    }

    async fn settle() {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    /// A repository that is reached over https with a credential, at a host nothing listens on.
    fn private_repo() -> RepoObservationConfig {
        RepoObservationConfig::new(
            RepoId::from_url("https://localhost:1/o/r.git"),
            "https://localhost:1/o/r.git".into(),
            Vec::new(),
            None,
            Vec::new(),
        )
        .with_credential("gh")
    }

    #[tokio::test]
    async fn a_repository_whose_credential_has_not_arrived_waits_and_clones_nothing() {
        let rig = start(private_repo()).await;
        until("the first attempt", || asked(&rig) == 1).await;
        settle().await;
        assert_eq!(asked(&rig), 1, "it asked again before anything changed");
        assert!(!rig.cache.path().join("clone").exists());
        assert!(
            published_commits(&rig.received).is_empty(),
            "no commit is published for a repository that was not cloned"
        );
        rig.worker.stop(None);
    }

    #[tokio::test]
    async fn a_waiting_repository_tries_again_at_once_when_the_credentials_change() {
        let rig = start(private_repo()).await;
        until("the first attempt", || asked(&rig) == 1).await;
        // Its frequency is 900 s, so only the message can make the second attempt happen now.
        rig.worker
            .cast(GitRepoWorkerMsg::CredentialsChanged)
            .unwrap();
        until("a second attempt", || asked(&rig) == 2).await;
        settle().await;
        assert_eq!(asked(&rig), 2, "one message made more than one attempt");
        rig.worker.stop(None);
    }

    #[tokio::test]
    async fn a_credential_that_has_arrived_is_used_for_the_clone_and_a_clone_that_fails_waits_for_the_next_attempt()
     {
        let url = "https://localhost:1/found/r.git";
        let rig = start_with(private_repo_at(url), refusing_clone).await;
        until("the first attempt", || asked(&rig) == 1).await;
        assert!(
            clones_of(url).is_empty(),
            "nothing is cloned before the credential is there"
        );
        *rig.crypto.answer.lock().unwrap() = Answer::Found {
            username: "svc",
            secret: b"tok",
        };
        rig.worker
            .cast(GitRepoWorkerMsg::CredentialsChanged)
            .unwrap();
        until("a second attempt", || asked(&rig) == 2).await;
        assert_eq!(
            rig.crypto.asked.lock().unwrap()[1],
            ("gh".to_string(), "localhost".to_string())
        );
        // The clone is made with a provider that offers the credential, and fails; the worker is still alive
        // and still waiting, so the next change tries the clone again.
        until("the clone", || clones_of(url).len() == 1).await;
        assert_eq!(clones_of(url), vec![true]);
        rig.worker
            .cast(GitRepoWorkerMsg::CredentialsChanged)
            .unwrap();
        until("a third attempt", || asked(&rig) == 3).await;
        until("the clone again", || clones_of(url).len() == 2).await;
        rig.worker.stop(None);
    }

    #[tokio::test]
    async fn a_public_repository_is_cloned_with_a_provider_that_offers_nothing() {
        let url = "https://localhost:1/public/r.git";
        let mut config = private_repo_at(url);
        config.credential = None;
        let rig = start_with(config, refusing_clone).await;
        until("the clone", || clones_of(url).len() == 1).await;
        assert_eq!(clones_of(url), vec![false]);
        assert_eq!(asked(&rig), 0);
        rig.worker.stop(None);
    }

    #[tokio::test]
    async fn a_credential_bound_to_another_host_is_not_used_and_nothing_is_cloned() {
        let rig = start(private_repo()).await;
        *rig.crypto.answer.lock().unwrap() = Answer::HostNotBound;
        until("the first attempt", || asked(&rig) >= 1).await;
        settle().await;
        assert!(!rig.cache.path().join("clone").exists());
        assert!(published_commits(&rig.received).is_empty());
        rig.worker.stop(None);
    }

    #[tokio::test]
    async fn a_public_repository_is_observed_without_asking_for_a_credential() {
        let origin = tempfile::tempdir().unwrap();
        let repo = Repository::init_bare(origin.path()).unwrap();
        let sig = git2::Signature::now("t", "t@example.org").unwrap();
        let tree = repo
            .find_tree(repo.treebuilder(None).unwrap().write().unwrap())
            .unwrap();
        let oid = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "first", &tree, &[])
            .unwrap();
        repo.set_head("refs/heads/main").unwrap();
        let url = origin.path().to_string_lossy().to_string();
        let config = RepoObservationConfig::new(
            RepoId::from_url(&url),
            url,
            vec!["origin".into()],
            None,
            vec!["refs/remotes/origin/main".into()],
        );
        let rig = start(config).await;
        until("the commit to be published", || {
            !rig.received.lock().unwrap().is_empty()
        })
        .await;
        assert_eq!(asked(&rig), 0);
        assert!(rig.cache.path().join("clone").exists());
        let _ = oid;
        rig.worker.stop(None);
    }

    // Paused time, so that "the old timer would have fired by now" is exact and not a race.
    #[tokio::test(start_paused = true)]
    async fn an_attempt_made_early_replaces_the_scheduled_one_instead_of_adding_to_it() {
        let mut config = private_repo();
        config.frequency = 10;
        let rig = start(config).await;
        until("the first attempt", || asked(&rig) == 1).await;
        tokio::time::advance(std::time::Duration::from_secs(3)).await;
        rig.worker
            .cast(GitRepoWorkerMsg::CredentialsChanged)
            .unwrap();
        until("the early attempt", || asked(&rig) == 2).await;
        // The first schedule would fire at 10 s; the early attempt moved it to 13 s.
        tokio::time::advance(std::time::Duration::from_secs(8)).await;
        settle().await;
        assert_eq!(asked(&rig), 2, "the earlier schedule still fired");
        tokio::time::advance(std::time::Duration::from_secs(3)).await;
        until("the rescheduled attempt", || asked(&rig) == 3).await;
        rig.worker.stop(None);
    }

    fn set(urls: &[&str], complete: bool) -> RepositorySet {
        RepositorySet {
            repositories: urls.iter().map(|u| private_repo_at(u)).collect(),
            complete,
        }
    }

    /// How many workers there are: each one that is waiting for its credential asks once when told they changed.
    async fn workers_in(rig: &SupervisorRig) -> usize {
        settle().await;
        let before = rig.crypto.asked.lock().unwrap().len();
        rig.supervisor
            .cast(RepoSupervisorMessage::CredentialsChanged)
            .unwrap();
        settle().await;
        rig.crypto.asked.lock().unwrap().len() - before
    }

    fn reconcile(rig: &SupervisorRig, set: RepositorySet) {
        rig.supervisor
            .cast(RepoSupervisorMessage::Reconcile { set })
            .unwrap();
    }

    #[tokio::test]
    async fn a_set_starts_a_worker_for_each_repository_in_it() {
        let rig = start_supervisor().await;
        reconcile(
            &rig,
            set(
                &[
                    "https://localhost:1/set/a.git",
                    "https://localhost:1/set/b.git",
                    "https://localhost:1/set/c.git",
                ],
                false,
            ),
        );
        assert_eq!(workers_in(&rig).await, 3);
        rig.supervisor.stop(None);
    }

    #[tokio::test]
    async fn the_same_set_again_changes_nothing_and_a_changed_repository_is_replaced() {
        let rig = start_supervisor().await;
        let urls = [
            "https://localhost:1/again/a.git",
            "https://localhost:1/again/b.git",
        ];
        reconcile(&rig, set(&urls, true));
        assert_eq!(workers_in(&rig).await, 2);
        let asks_before = rig.crypto.asked.lock().unwrap().len();
        reconcile(&rig, set(&urls, true));
        settle().await;
        assert_eq!(
            rig.crypto.asked.lock().unwrap().len(),
            asks_before,
            "an identical set started new workers"
        );
        let mut changed = set(&urls, true);
        changed.repositories[0].frequency = 901;
        reconcile(&rig, changed);
        settle().await;
        assert_eq!(
            rig.crypto.asked.lock().unwrap().len(),
            asks_before + 1,
            "only the changed repository should have a new worker"
        );
        assert_eq!(
            workers_in(&rig).await,
            2,
            "the replaced worker is still running"
        );
        rig.supervisor.stop(None);
    }

    #[tokio::test]
    async fn a_complete_set_stops_the_workers_it_does_not_name() {
        let rig = start_supervisor().await;
        reconcile(
            &rig,
            set(
                &[
                    "https://localhost:1/drop/a.git",
                    "https://localhost:1/drop/b.git",
                    "https://localhost:1/drop/c.git",
                ],
                true,
            ),
        );
        assert_eq!(workers_in(&rig).await, 3);
        reconcile(&rig, set(&["https://localhost:1/drop/b.git"], true));
        assert_eq!(workers_in(&rig).await, 1);
        rig.supervisor.stop(None);
    }

    #[tokio::test]
    async fn an_incomplete_set_stops_nothing_but_still_starts_what_is_new() {
        let rig = start_supervisor().await;
        reconcile(
            &rig,
            set(
                &[
                    "https://localhost:1/inc/a.git",
                    "https://localhost:1/inc/b.git",
                ],
                true,
            ),
        );
        assert_eq!(workers_in(&rig).await, 2);
        reconcile(&rig, set(&["https://localhost:1/inc/c.git"], false));
        assert_eq!(
            workers_in(&rig).await,
            3,
            "the two it did not name must survive, and c start"
        );
        rig.supervisor.stop(None);
    }

    #[tokio::test]
    async fn an_empty_set_stops_everything_only_if_it_is_complete() {
        let rig = start_supervisor().await;
        reconcile(
            &rig,
            set(
                &[
                    "https://localhost:1/empty/a.git",
                    "https://localhost:1/empty/b.git",
                ],
                true,
            ),
        );
        assert_eq!(workers_in(&rig).await, 2);
        reconcile(&rig, set(&[], false));
        assert_eq!(
            workers_in(&rig).await,
            2,
            "an incomplete empty set stopped workers"
        );
        reconcile(&rig, set(&[], true));
        assert_eq!(workers_in(&rig).await, 0);
        rig.supervisor.stop(None);
    }

    #[tokio::test]
    async fn a_repository_that_was_dropped_is_observed_again_when_a_later_set_names_it() {
        let rig = start_supervisor().await;
        let url = "https://localhost:1/back/a.git";
        reconcile(&rig, set(&[url], true));
        assert_eq!(workers_in(&rig).await, 1);
        reconcile(&rig, set(&[], true));
        assert_eq!(workers_in(&rig).await, 0);
        reconcile(&rig, set(&[url], true));
        assert_eq!(workers_in(&rig).await, 1);
        rig.supervisor.stop(None);
    }
}
