//! What the git processor writes to the graph for each event the observer publishes, as values, so
//! that what is written can be checked without a graph.

use chrono::Utc;
use git_agent_common::GitRepositoryMessage;
use polar::graph::controller::{GraphOp, GraphValue, IntoGraphKey, Property, rel};
use polar::graph::nodes::git::GitNodeKey;
use tracing::{trace, warn};

/// A discovered commit: immutable commit data and topology. It never touches refs.
pub fn for_commit_discovered(ev: GitRepositoryMessage) -> Vec<GraphOp> {
    let GitRepositoryMessage::CommitDiscovered {
        repo,
        oid,
        time,
        message,
        committer,
        parents,
        ..
    } = ev
    else {
        warn!("Received unexpected event {ev:?}");
        return vec![];
    };
    let repo_key = GitNodeKey::Repository { repo_id: repo };
    let commit_key = GitNodeKey::Commit { oid: oid.clone() };

    let mut ops = vec![
        GraphOp::UpsertNode {
            key: repo_key.clone().into_key(),
            props: vec![],
        },
        GraphOp::UpsertNode {
            key: commit_key.clone().into_key(),
            props: vec![
                Property("message".into(), GraphValue::String(message)),
                Property("authored_time".into(), GraphValue::I64(time)),
                Property("committer".into(), GraphValue::String(committer)),
                Property(
                    "observed_at".into(),
                    GraphValue::I64(Utc::now().timestamp_millis()),
                ),
            ],
        },
        GraphOp::EnsureEdge {
            from: repo_key.into_key(),
            to: commit_key.clone().into_key(),
            rel_type: rel::CONTAINS.into(),
            props: vec![],
        },
    ];
    for parent_oid in parents {
        if parent_oid == oid {
            trace!("Skipping self-parent edge for commit {oid}");
            continue;
        }
        ops.push(GraphOp::EnsureEdge {
            from: commit_key.clone().into_key(),
            to: GitNodeKey::Commit { oid: parent_oid }.into_key(),
            rel_type: "PARENT".into(),
            props: vec![],
        });
    }
    ops
}

/// A ref update: the authoritative source for ref to commit relationships. Earlier `POINTS_TO`
/// edges stay, so a ref keeps the history of where it pointed.
pub fn for_ref_updated(ev: GitRepositoryMessage) -> Vec<GraphOp> {
    let GitRepositoryMessage::RefUpdated {
        repo,
        ref_name,
        new,
        observed_at,
        ..
    } = ev
    else {
        warn!("Received unexpected event {ev:?}");
        return vec![];
    };
    let ref_key = GitNodeKey::Ref {
        repo_id: repo.clone(),
        name: ref_name,
    };
    let commit_key = GitNodeKey::Commit { oid: new };
    vec![
        GraphOp::UpsertNode {
            key: GitNodeKey::Repository { repo_id: repo }.into_key(),
            props: vec![],
        },
        GraphOp::UpsertNode {
            key: ref_key.clone().into_key(),
            props: vec![],
        },
        GraphOp::UpsertNode {
            key: commit_key.clone().into_key(),
            props: vec![],
        },
        GraphOp::EnsureEdge {
            from: ref_key.into_key(),
            to: commit_key.into_key(),
            rel_type: rel::POINTS_TO.into(),
            props: vec![Property(
                "observed_at".into(),
                GraphValue::String(observed_at),
            )],
        },
    ]
}

/// The operations for any event the observer publishes.
pub fn for_event(ev: GitRepositoryMessage) -> Vec<GraphOp> {
    match ev {
        GitRepositoryMessage::CommitDiscovered { .. } => for_commit_discovered(ev),
        GitRepositoryMessage::RefUpdated { .. } => for_ref_updated(ev),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar::graph::nodes::git::RepoId;

    fn commit(oid: &str, parents: &[&str]) -> GitRepositoryMessage {
        GitRepositoryMessage::CommitDiscovered {
            repo: RepoId::new("r".into()),
            ref_name: "refs/heads/main".into(),
            oid: oid.into(),
            committer: "c".into(),
            time: 1,
            message: "m".into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            observed_at: "1".into(),
        }
    }

    fn rels(ops: &[GraphOp]) -> Vec<String> {
        ops.iter()
            .filter_map(|op| match op {
                GraphOp::EnsureEdge { rel_type, .. } => Some(rel_type.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_commit_is_contained_by_its_repository_and_points_at_its_parents() {
        let ops = for_event(commit("b", &["a", "z"]));
        assert_eq!(rels(&ops), ["CONTAINS", "PARENT", "PARENT"]);
    }

    #[test]
    fn a_commit_that_names_itself_as_a_parent_gets_no_edge_to_itself() {
        let ops = for_event(commit("b", &["b", "a"]));
        assert_eq!(rels(&ops), ["CONTAINS", "PARENT"]);
    }

    #[test]
    fn a_ref_update_points_the_ref_at_the_new_commit() {
        let ops = for_event(GitRepositoryMessage::RefUpdated {
            repo: RepoId::new("r".into()),
            ref_name: "refs/heads/main".into(),
            old: Some("a".into()),
            new: "b".into(),
            observed_at: "2".into(),
        });
        assert_eq!(rels(&ops), ["POINTS_TO"]);
    }
}
