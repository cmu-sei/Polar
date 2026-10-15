use polar::graph::nodes::git::RepoId;
use rkyv::{Archive, Deserialize, Serialize};

pub const GIT_REPO_CONFIG_EVENTS: &str = "git.repo.config.events";
pub const GIT_REPO_EVENTS: &str = "git.repo.events";
pub const GIT_REPO_PROCESSING_TOPIC: &str = "polar.git.processor";

/* ============================
 * Shared types
 * ============================
 */

/// Observation policy applied to a repository.
///
/// This is *policy*, not state.
/// It must be supplied externally (scheduler, config, graph).
///
/// A repository that needs a credential names it (`credential`); it never carries one. The observer
/// holds the credentials it was given in a signed `credentials` document, in its CryptoActor, and
/// uses one only for the hosts that credential is bound to. `None` means a public repository:
/// unauthenticated access.
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
    /// The name of the credential to authenticate to the remote with, as the observer's `credentials`
    /// section names it. None means a public repository: unauthenticated clone and fetch.
    pub credential: Option<String>,
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
            credential: None,
        }
    }

    /// The same repository, to be reached with the credential called `name`.
    pub fn with_credential(mut self, name: impl Into<String>) -> Self {
        self.credential = Some(name.into());
        self
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

/// Publishes an event on a topic, as every git agent does: rkyv bytes, identity shared, queued if
/// the broker is away.
pub fn publish_event<T>(
    client: &polar::cassini::TcpClient,
    topic: &str,
    event: &T,
) -> Result<(), String>
where
    T: for<'a> rkyv::Serialize<
            rkyv::api::high::HighSerializer<
                rkyv::util::AlignedVec,
                rkyv::ser::allocator::ArenaHandle<'a>,
                rkyv::rancor::Error,
            >,
        >,
{
    use polar::cassini::CassiniClient;
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(event)
        .map_err(|e| e.to_string())?
        .to_vec();
    client
        .publish(cassini_client::PublishRequest {
            topic: topic.to_string(),
            payload,
            trace_ctx: None,
            offline_behavior: cassini_client::OfflineBehavior::default(),
            share_identity: true,
        })
        .map_err(|e| e.to_string())
}
