use polar::graph::nodes::git::RepoId;
use rkyv::{Archive, Deserialize, Serialize};

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
