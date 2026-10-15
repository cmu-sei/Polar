#![deny(clippy::disallowed_methods)]

use git_agent_common::GitRepositoryMessage;
use git_agent_common::RepoObservationConfig;
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

pub mod agent;
pub mod credentials;
pub mod settings;
pub mod supervisor;

fn git_time_to_epoch_ms(t: &git2::Time) -> i64 {
    t.seconds() * 1000
}

/* ============================
 * Shared types
 * ============================
 */

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
    crypto: Arc<dyn CryptoClient>,
}

pub struct GitRepoSupervisorArgs {
    cache_root: PathBuf,
    tcp_client: TcpClient,
    /// Where each worker asks for the credential its repository names.
    crypto: Arc<dyn CryptoClient>,
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

                let args = GitRepoWorkerArgs {
                    config,
                    crypto: state.crypto.clone(),
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
                    Err(e) => tolerate_spawn_failure(&repo_id, e)?,
                }
            }
            RepoSupervisorMessage::CredentialsChanged => {
                for worker in state.workers.values() {
                    if let Err(e) = worker.cast(GitRepoWorkerMsg::CredentialsChanged) {
                        warn!("could not tell a worker the credentials changed: {e}");
                    }
                }
            }
            RepoSupervisorMessage::StopWorker { repo_id } => {
                // The clone stays in the cache: it is reused if the repository is observed again.
                match state.workers.remove(&repo_id) {
                    Some(worker) => worker.stop(Some("asked to stop".into())),
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
    /// Cloned or opened by the first attempt that has what it needs, not before: a repository whose
    /// credential has not arrived has no clone yet.
    repo: Option<Repository>,
    last_seen: HashMap<String, Oid>,
    /// The last attempt could not go ahead (its credential is missing or unusable, or the clone failed).
    waiting: bool,
    /// The next scheduled attempt, replaced whenever one runs so there is never more than one.
    next: Option<tokio::task::JoinHandle<Result<(), ractor::MessagingErr<GitRepoWorkerMsg>>>>,
}

pub struct GitRepoWorkerArgs {
    config: RepoObservationConfig,
    repo_path: PathBuf,
    tcp_client: TcpClient,
    crypto: Arc<dyn CryptoClient>,
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
    /// missing or unusable, or the clone failed, is logged and leaves the worker waiting for the next
    /// (the next fetch time, or the credentials changing); it is not an error. A failure of the fetch
    /// itself is, as it always was.
    async fn attempt(&self, state: &mut GitRepoWorkerState) -> Result<(), ActorProcessingErr> {
        let provider = match credentials::resolve(&*state.crypto, &state.config).await {
            credentials::Resolution::Ready(provider) => provider,
            credentials::Resolution::Pending => {
                info!(
                    "{} is waiting for the credential it names",
                    state.config.repo_id
                );
                state.waiting = true;
                return Ok(());
            }
            credentials::Resolution::Failed(why) => {
                warn!("{} is not observed: {why}", state.config.repo_id);
                state.waiting = true;
                return Ok(());
            }
        };

        if state.repo.is_none() {
            match Self::open_or_clone_bare(
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
                    return Ok(());
                }
            }
        }
        state.waiting = false;

        let Some(repo) = state.repo.as_ref() else {
            return Ok(());
        };
        Self::observe_repository(
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
        )?;
        Ok(())
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
        Ok(GitRepoWorkerState {
            config: args.config,
            repo_path: args.repo_path,
            tcp_client: args.tcp_client,
            crypto: args.crypto,
            repo: None,
            last_seen: HashMap::new(),
            waiting: false,
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

                self.attempt(state).await?;

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

    async fn start(config: RepoObservationConfig) -> Rig {
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
        assert!(rig.received.lock().unwrap().is_empty());
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
        let rig = start(private_repo()).await;
        until("the first attempt", || asked(&rig) == 1).await;
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
        // Nothing listens at the host, so the clone fails; the worker is still alive and still waiting.
        settle().await;
        rig.worker
            .cast(GitRepoWorkerMsg::CredentialsChanged)
            .unwrap();
        until("a third attempt", || asked(&rig) == 3).await;
        rig.worker.stop(None);
    }

    #[tokio::test]
    async fn a_credential_bound_to_another_host_is_not_used_and_nothing_is_cloned() {
        let rig = start(private_repo()).await;
        *rig.crypto.answer.lock().unwrap() = Answer::HostNotBound;
        until("the first attempt", || asked(&rig) >= 1).await;
        settle().await;
        assert!(!rig.cache.path().join("clone").exists());
        assert!(rig.received.lock().unwrap().is_empty());
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
}
