//! What the git processor writes to the graph for each event the observer publishes, as values, so
//! that what is written can be checked without a graph.

use chrono::Utc;
use git_agent_common::GitRepositoryMessage;
use git_agent_common::status::ObservationStatus;
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

/// What the observer says became of its latest attempt at a repository, written onto the repository's node so a
/// person looking at the graph can tell a repository that is observed from one that is waiting or failing.
///
/// The properties are the latest word, replaced by the next. `observation_reason` is cleared (set to null) when
/// the attempt had none, so an error does not outlive the failure it explained. `observation_last_success` is
/// written only when the observer knows one: a replaced observer does not know when its predecessor's was, and
/// must not erase it.
pub fn for_status(status: ObservationStatus) -> Vec<GraphOp> {
    let mut props = vec![
        Property(
            "observation_state".into(),
            GraphValue::String(status.state.as_str().into()),
        ),
        Property(
            "observation_reason".into(),
            match status.reason {
                Some(reason) => GraphValue::String(reason),
                None => GraphValue::Null,
            },
        ),
        Property(
            "observation_consecutive_failures".into(),
            GraphValue::I64(i64::from(status.consecutive_failures)),
        ),
        Property(
            "observation_checked_at".into(),
            GraphValue::I64(status.observed_at),
        ),
    ];
    if let Some(at) = status.last_success {
        props.push(Property(
            "observation_last_success".into(),
            GraphValue::I64(at),
        ));
    }
    vec![GraphOp::UpsertNode {
        key: GitNodeKey::Repository {
            repo_id: status.repo_id,
        }
        .into_key(),
        props,
    }]
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
    use git_agent_common::status::ObservationState;
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

    fn status(
        state: ObservationState,
        reason: Option<&str>,
        last_success: Option<i64>,
    ) -> ObservationStatus {
        ObservationStatus {
            repo_id: RepoId::new("r".into()),
            state,
            reason: reason.map(String::from),
            last_success,
            tips: vec![("refs/remotes/origin/main".into(), "abc".into())],
            consecutive_failures: 2,
            observed_at: 99,
        }
    }

    fn props_of(ops: &[GraphOp]) -> Vec<(String, GraphValue)> {
        match ops {
            [GraphOp::UpsertNode { props, .. }] => props
                .iter()
                .map(|Property(k, v)| (k.to_string(), v.clone()))
                .collect(),
            other => panic!("expected one UpsertNode, got {other:?}"),
        }
    }

    fn prop(props: &[(String, GraphValue)], name: &str) -> Option<GraphValue> {
        props
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn a_status_is_written_onto_the_repository_node_and_nothing_else() {
        let ops = for_status(status(
            ObservationState::Failing,
            Some("the clone failed: x"),
            Some(7),
        ));
        match ops.as_slice() {
            [GraphOp::UpsertNode { key, .. }] => {
                let (pattern, _) = GitNodeKey::Repository {
                    repo_id: RepoId::new("r".into()),
                }
                .into_key()
                .cypher_match("n");
                let (written, _) = key.cypher_match("n");
                assert_eq!(written, pattern);
                assert!(written.contains("GitRepository"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_state_the_reason_the_count_and_the_time_are_written() {
        let props = props_of(&for_status(status(
            ObservationState::Failing,
            Some("the fetch failed: down"),
            Some(7),
        )));
        assert_eq!(
            prop(&props, "observation_state"),
            Some(GraphValue::String("failing".into()))
        );
        assert_eq!(
            prop(&props, "observation_reason"),
            Some(GraphValue::String("the fetch failed: down".into()))
        );
        assert_eq!(
            prop(&props, "observation_consecutive_failures"),
            Some(GraphValue::I64(2))
        );
        assert_eq!(
            prop(&props, "observation_checked_at"),
            Some(GraphValue::I64(99))
        );
        assert_eq!(
            prop(&props, "observation_last_success"),
            Some(GraphValue::I64(7))
        );
    }

    #[test]
    fn an_old_reason_is_cleared_when_there_is_none_now() {
        let props = props_of(&for_status(status(
            ObservationState::Observing,
            None,
            Some(7),
        )));
        assert_eq!(
            prop(&props, "observation_reason"),
            Some(GraphValue::Null),
            "absent would leave the old error on the node"
        );
        assert_eq!(
            prop(&props, "observation_state"),
            Some(GraphValue::String("observing".into()))
        );
    }

    #[test]
    fn a_last_success_the_observer_does_not_know_is_not_written_so_the_graph_keeps_its_own() {
        let props = props_of(&for_status(status(
            ObservationState::PendingCredential,
            None,
            None,
        )));
        assert_eq!(prop(&props, "observation_last_success"), None);
        assert_eq!(
            prop(&props, "observation_state"),
            Some(GraphValue::String("pending-credential".into()))
        );
    }
}
