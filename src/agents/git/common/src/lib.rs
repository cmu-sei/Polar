use polar::graph::nodes::git::RepoId;
use rkyv::{Archive, Deserialize, Serialize};

pub const GIT_REPO_CONFIG_EVENTS: &str = "git.repo.config.events";
pub const GIT_REPO_EVENTS: &str = "git.repo.events";
pub const GIT_REPO_PROCESSING_TOPIC: &str = "polar.git.processor";

/* ============================
 * Credentials
 * ============================
 */

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
    pub fn into_git2_cred(self) -> Result<git2::Cred, git2::Error> {
        match self {
            GitHttpCredential::UserPass { username, password } => {
                git2::Cred::userpass_plaintext(&username, &password)
            }
            GitHttpCredential::Token { token, username } => {
                git2::Cred::userpass_plaintext(username.as_deref().unwrap_or("oauth2"), &token)
            }
        }
    }
}

#[derive(Serialize, Deserialize, Archive, Debug)]
pub struct ConfigurationEvent {
    pub config: RepoObservationConfig,
}

#[derive(Serialize, Deserialize, Archive, Debug)]
pub enum GitRepositoryMessage {
    /// Emitted once per commit per repo, ever.
    /// - "This commit exists"
    /// - "Here is its immutable metadata"
    /// - "Here is its position in the DAG"
    ///
    /// Downstream:
    ///  1. Create Commit node
    ///  2. Create PARENT edges
    CommitDiscovered {
        repo: RepoId,
        ref_name: String,
        oid: String,
        // TODO: Add author fields
        // Names and emails might not always be set, and we're not creating nodes for user identity (yet).
        // So we can leave them out for now.
        // author: String,
        // author_email: String,
        committer: String,
        time: i64,
        message: String,
        parents: Vec<String>,
        observed_at: String,
    },

    /// Semantics:
    /// - "This pointer now points here"
    /// - Force-pushes become explicit, not magical
    ///
    /// Downstream:
    ///  1. Update or version REF_POINTS_TO
    ///  2. Optionally keep history of ref movement
    ///
    /// **Important!!!**
    /// **This event should be emitted after commits are discovered, or at least be reorder-tolerant.**
    RefUpdated {
        repo: RepoId,
        ref_name: String,
        old: Option<String>,
        new: String,
        observed_at: String,
    },
}
