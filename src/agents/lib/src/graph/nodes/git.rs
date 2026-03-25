use neo4rs::BoltType;
use rkyv::{Archive, Deserialize, Serialize};

use crate::graph::controller::GraphNodeKey;

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

    pub fn new(url: String) -> Self {
        RepoId(url)
    }
}

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone)]
pub enum GitNodeKey {
    Repository {
        repo_id: RepoId, // your canonical UUID / v5
    },

    Commit {
        oid: String, // full hex
    },

    Ref {
        repo_id: RepoId,
        name: String, // refs/heads/main, refs/tags/v1.2.3
    },

    Author {
        email: String,
    },
}

impl GraphNodeKey for GitNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            GitNodeKey::Repository { repo_id } => (
                format!("({prefix}:GitRepository {{ id: ${prefix}_repo_id }})"),
                vec![(format!("{prefix}_repo_id"), repo_id.to_string().into())],
            ),

            GitNodeKey::Commit { oid } => (
                format!("({prefix}:GitCommit {{ oid: ${prefix}_oid }})"),
                vec![(format!("{prefix}_oid"), oid.clone().into())],
            ),

            GitNodeKey::Ref { repo_id, name } => (
                format!("({prefix}:GitRef {{ repo_id: ${prefix}_repo_id, name: ${prefix}_name }})"),
                vec![
                    (format!("{prefix}_repo_id"), repo_id.to_string().into()),
                    (format!("{prefix}_name"), name.clone().into()),
                ],
            ),
            GitNodeKey::Author { email } => (
                format!("({prefix}:GitAuthor {{ email: ${prefix}_email }})"),
                vec![
                    (format!("{prefix}_email"), email.clone().into()),
                    // name should be SET, not matched on
                ],
            ),
        }
    }
}

// === Supervisor state ===
