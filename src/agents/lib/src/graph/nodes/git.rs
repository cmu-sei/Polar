/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

use neo4rs::BoltType;
use rkyv::{Archive, Deserialize, Serialize};

use crate::graph::controller::{GraphNodeKey, NodeIdentity, NodeKeyVocabulary};

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

    /// Canonicalize a repo URL for use as a stable identity / config key.
    /// Lowercases scheme and host (path stays case-sensitive — org/repo names
    /// on GitHub, GitLab, and Gitea are case-sensitive), strips a trailing
    /// `.git` suffix and trailing slash.
    pub fn normalize_repo_url(url: &str) -> String {
        let mut s = url.trim().trim_end_matches('/').to_string();
        if let Some(stripped) = s.strip_suffix(".git") {
            s = stripped.to_string();
        }
        match url::Url::parse(&s) {
            Ok(parsed) => {
                let scheme = parsed.scheme().to_lowercase();
                let host = parsed.host_str().unwrap_or_default().to_lowercase();
                let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
                format!("{scheme}://{host}{port}{}", parsed.path())
            }
            Err(_) => s,
        }
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

    /// One proposer's proposal that a repository be observed, and what the git controller decided about it. The
    /// id is the repository's id and the proposer's identity, so a proposal announced again is the same node.
    Proposal {
        id: String,
    },
}

impl GraphNodeKey for GitNodeKey {
    fn identity(&self) -> NodeIdentity {
        let s = |v: &str| BoltType::String(v.to_string().into());
        match self {
            GitNodeKey::Repository { repo_id } => {
                NodeIdentity::fixed(&["GitRepository"], vec![("id", s(&repo_id.to_string()))])
            }
            GitNodeKey::Commit { oid } => {
                NodeIdentity::fixed(&["GitCommit"], vec![("oid", s(oid))])
            }
            GitNodeKey::Ref { repo_id, name } => NodeIdentity::fixed(
                &["GitRef"],
                vec![("repo_id", s(&repo_id.to_string())), ("name", s(name))],
            ),
            GitNodeKey::Proposal { id } => {
                NodeIdentity::fixed(&["GitProposal"], vec![("id", s(id))])
            }
            // name should be SET, not matched on
            GitNodeKey::Author { email } => {
                NodeIdentity::fixed(&["GitAuthor"], vec![("email", s(email))])
            }
        }
    }
}

impl NodeKeyVocabulary for GitNodeKey {
    fn every_variant() -> Vec<Self> {
        let s = || "x".to_string();
        let all = vec![
            GitNodeKey::Repository {
                repo_id: RepoId::new(s()),
            },
            GitNodeKey::Commit { oid: s() },
            GitNodeKey::Ref {
                repo_id: RepoId::new(s()),
                name: s(),
            },
            GitNodeKey::Author { email: s() },
            GitNodeKey::Proposal { id: s() },
        ];
        // A variant added to the enum does not compile until it is named here, beside the list it must join.
        for key in &all {
            match key {
                GitNodeKey::Repository { .. }
                | GitNodeKey::Commit { .. }
                | GitNodeKey::Ref { .. }
                | GitNodeKey::Author { .. }
                | GitNodeKey::Proposal { .. } => {}
            }
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proposal_is_matched_by_its_id_under_its_own_label() {
        let (pattern, params) = GitNodeKey::Proposal {
            id: "https_github.com_o_r|jira".into(),
        }
        .cypher_match("p");
        assert_eq!(pattern, "(p:GitProposal { id: $p_id })");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].0, "p_id");
    }

    #[test]
    fn a_proposal_is_not_a_repository_and_does_not_collide_with_one() {
        let (proposal, _) = GitNodeKey::Proposal { id: "x".into() }.cypher_match("n");
        let (repository, _) = GitNodeKey::Repository {
            repo_id: RepoId::new("x".into()),
        }
        .cypher_match("n");
        assert_ne!(proposal, repository);
        assert!(proposal.contains("GitProposal") && !proposal.contains("GitRepository"));
    }
}
