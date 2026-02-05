use git2::Oid;
use rkyv::{Archive, Deserialize, Serialize};

pub const GIT_REPO_CONFIG_REQUESTS: &str = "git.repo.config.requests";
pub const GIT_REPO_CONFIG_RESPONSES: &str = "git.repo.config.responses";
pub const GIT_REPO_EVENTS: &str = "git.repo.events";
/* ============================
 * Shared types
 * ============================
 */

/// Observation policy applied to a repository.
///
/// This is *policy*, not state.
/// It must be supplied externally (scheduler, config, graph).
#[derive(Serialize, Deserialize, Archive, Debug, Clone)]
pub struct RepoObservationConfig {
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
    /// TODO: should default to 100
    pub shallow_depth: Option<usize>,

    /// Frequency of observation in seconds.
    pub frequency: u64,
}

impl RepoObservationConfig {
    /// A default value for the configuration.
    /// We want to be reasonable and not too aggressive, git repositories can incredibly large, with chains that go back a very long time.
    /// The can also be updated infrequently. So we default to a reasonable value for the frequency we check them and the depth we walk.
    pub fn new(
        repo_url: String,
        remotes: Vec<String>,
        shallow_depth: Option<usize>,
        refs: Vec<String>,
    ) -> Self {
        Self {
            repo_url,
            remotes,
            refs,
            max_depth: 100,
            shallow_depth,
            frequency: 900,
        }
    }
}

/// Canonical identifier for a repository.
///
/// This MUST be stable and collision-resistant.
/// URL normalization or UUIDv5 are both acceptable.
#[derive(Serialize, Deserialize, Archive, Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoId(String);

impl RepoId {
    pub fn from_url(url: &str) -> Self {
        // Opinionated but deterministic.
        // Replace with UUIDv5 if you want cryptographic guarantees.
        Self(url.replace("://", "_").replace('/', "_"))
    }
    pub fn to_string(&self) -> String {
        self.0.clone()
    }
}

#[derive(Serialize, Deserialize, Archive, Debug)]
pub enum GitRepositoryMessage {
    ConfigurationRequest {
        repo_url: String,
    },
    ConfigurationResponse {
        config: RepoObservationConfig,
    },

    ///Emitted once per commit per repo, ever.
    /// - “This commit exists”
    /// - “Here is its immutable metadata”
    /// - “Here is its position in the DAG”
    ///
    ///Downstream:
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
    /// - “This pointer now points here”
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

    RepositoryObserved {
        repo: RepoId,
        observed_at: i64,
    },
}
