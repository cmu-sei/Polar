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

/* ============================
 * Tests
 * ============================
 */

use git_agent_common::RepoObservationConfig;
use git_repo_observer::*;
use git2::{Commit, CredentialType, Oid, Repository};
use polar::graph::nodes::git::RepoId;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Once;
use tempfile::TempDir;

static INIT_LOGGING: Once = Once::new();

fn init_logging() {
    INIT_LOGGING.call_once(|| {
        polar::init_logging(SERVICE_NAME.to_string());
    });
}

fn dummy_config(refs: Vec<String>) -> RepoObservationConfig {
    let id = RepoId::from_url("local");
    RepoObservationConfig::new(id, "local".into(), Vec::new(), Some(100), refs)
}

fn bare_repo() -> (TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(dir.path()).unwrap();
    (dir, repo)
}

fn noop_provider() -> Arc<dyn GitCredentialProvider> {
    Arc::new(PublicRepoProvider)
}

fn make_commit(repo: &Repository, message: &str, parents: &[&Commit]) -> Oid {
    let sig = git2::Signature::now("tester", "tester@example.com").unwrap();
    let tree_id = repo.treebuilder(None).unwrap().write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(None, &sig, &sig, message, &tree, parents)
        .unwrap()
}

fn linear_history(repo: &Repository, ref_name: &str, count: usize) -> Vec<Oid> {
    let mut commits = Vec::new();
    for i in 0..count {
        let parents = commits
            .last()
            .and_then(|oid| repo.find_commit(*oid).ok())
            .into_iter()
            .collect::<Vec<_>>();
        let oid = make_commit(repo, &format!("c{i}"), &parents.iter().collect::<Vec<_>>());
        commits.push(oid);
    }
    repo.reference(ref_name, *commits.last().unwrap(), true, "init")
        .unwrap();
    commits
}

// --- Providers ---

#[test]
fn public_repo_provider_always_errors() {
    let provider = PublicRepoProvider;
    let result = provider.credentials(
        "https://github.com/org/public-repo.git",
        None,
        CredentialType::USER_PASS_PLAINTEXT,
    );
    assert!(result.is_err());
}

// --- Walker / observer tests (unchanged from before) ---

#[test]
fn walk_without_last_seen_returns_newest_first() {
    init_logging();
    let (_dir, repo) = bare_repo();
    let commits = linear_history(&repo, "refs/heads/main", 3);
    let mut seen = Vec::new();
    GitRepoWorker::walk_commits_incremental(&repo, "refs/heads/main", None, 10, |c| {
        seen.push(c.id())
    })
    .unwrap();
    assert_eq!(seen, commits.iter().rev().copied().collect::<Vec<_>>());
}

#[test]
fn walk_respects_max_depth() {
    init_logging();
    let (_dir, repo) = bare_repo();
    let commits = linear_history(&repo, "refs/heads/main", 5);
    let mut seen = Vec::new();
    GitRepoWorker::walk_commits_incremental(&repo, "refs/heads/main", None, 2, |c| {
        seen.push(c.id())
    })
    .unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0], commits[4]);
    assert_eq!(seen[1], commits[3]);
}

#[test]
fn walk_nonexistent_ref_errors_cleanly() {
    init_logging();
    let (_dir, repo) = bare_repo();
    let err = GitRepoWorker::walk_commits_incremental(&repo, "refs/heads/nope", None, 10, |_| {})
        .expect_err("expected error");
    assert!(
        err.message().contains("reference"),
        "unexpected error: {err:?}"
    );
}

#[test]
fn observe_updates_last_seen_per_ref_independently() {
    init_logging();
    let (_dir, repo) = bare_repo();
    let main_commits = linear_history(&repo, "refs/heads/main", 2);
    let dev_commits = linear_history(&repo, "refs/heads/dev", 3);
    let mut last_seen = HashMap::new();
    let mut seen = Vec::new();
    let mut refs_seen = HashMap::new();
    let obs_config = dummy_config(vec!["refs/heads/main".into(), "refs/heads/dev".into()]);
    let provider = noop_provider();
    GitRepoWorker::observe_repository(
        &repo,
        &provider,
        &obs_config,
        &mut last_seen,
        |r, c| seen.push((r.to_string(), c.id())),
        |ref_name, old, new| {
            refs_seen.insert(ref_name.to_string(), (old, new));
        },
    )
    .unwrap();
    assert_eq!(last_seen["refs/heads/main"], *main_commits.last().unwrap());
    assert_eq!(last_seen["refs/heads/dev"], *dev_commits.last().unwrap());
}

#[test]
fn multiple_refs_do_not_interfere() {
    init_logging();
    let (_dir, repo) = bare_repo();
    let main_commits = linear_history(&repo, "refs/heads/main", 1);
    let dev_commits = linear_history(&repo, "refs/heads/dev", 1);
    let mut last_seen = HashMap::from([
        ("refs/heads/main".into(), main_commits[0]),
        ("refs/heads/dev".into(), dev_commits[0]),
    ]);
    let new_dev = make_commit(
        &repo,
        "dev-2",
        &[&repo.find_commit(dev_commits[0]).unwrap()],
    );
    repo.reference("refs/heads/dev", new_dev, true, "update")
        .unwrap();
    let config = dummy_config(vec!["refs/heads/dev".into()]);
    let provider = noop_provider();
    GitRepoWorker::observe_repository(
        &repo,
        &provider,
        &config,
        &mut last_seen,
        |_r, _c| {},
        |_ref_name, _old, _new| {},
    )
    .unwrap();
    assert_eq!(last_seen["refs/heads/dev"], new_dev);
    assert_eq!(last_seen["refs/heads/main"], main_commits[0]);
}

#[test]
fn force_push_emits_ref_updated_with_old_and_new_tips() {
    init_logging();
    let (_dir, repo) = bare_repo();

    let first_history = linear_history(&repo, "refs/heads/main", 3);
    let old_tip = *first_history.last().unwrap();

    let mut last_seen = HashMap::new();
    let config = dummy_config(vec!["refs/heads/main".into()]);
    let provider = noop_provider();

    GitRepoWorker::observe_repository(
        &repo,
        &provider,
        &config,
        &mut last_seen,
        |_, _| {},
        |_, _, _| {},
    )
    .unwrap();
    assert_eq!(last_seen["refs/heads/main"], old_tip);

    // Force-push: a disjoint replacement history with no shared ancestors,
    // forced onto main.
    let new_root = make_commit(&repo, "rewritten-root", &[]);
    let new_root_commit = repo.find_commit(new_root).unwrap();
    let new_tip = make_commit(&repo, "rewritten-head", &[&new_root_commit]);
    repo.reference("refs/heads/main", new_tip, true, "force-push")
        .unwrap();

    let mut commits_seen = Vec::new();
    let mut refs_updated = Vec::new();

    GitRepoWorker::observe_repository(
        &repo,
        &provider,
        &config,
        &mut last_seen,
        |r, c| commits_seen.push((r.to_string(), c.id())),
        |ref_name, old, new| refs_updated.push((ref_name.to_string(), old, new)),
    )
    .unwrap();

    assert_eq!(
        refs_updated,
        vec![("refs/heads/main".to_string(), Some(old_tip), new_tip)]
    );
    assert_eq!(last_seen["refs/heads/main"], new_tip);

    let new_oids: Vec<_> = commits_seen.iter().map(|(_, oid)| *oid).collect();
    assert!(new_oids.contains(&new_root));
    assert!(new_oids.contains(&new_tip));
    assert!(!new_oids.contains(&old_tip));
}
// One repository that cannot be started (an unreachable host, bad credentials) must not end the
// supervisor of all the others: its failure is logged and the supervisor carries on.
#[test]
fn a_worker_that_fails_to_start_does_not_fail_the_supervisor() {
    let repo = RepoId::new("unreachable".into());
    let failed = ractor::SpawnErr::StartupFailed("failed to resolve address".into());
    assert!(git_repo_observer::tolerate_spawn_failure(&repo, failed).is_ok());
}

#[test]
fn a_worker_that_is_already_running_is_not_an_error() {
    let repo = RepoId::new("again".into());
    let dup = ractor::SpawnErr::ActorAlreadyRegistered("worker".into());
    assert!(git_repo_observer::tolerate_spawn_failure(&repo, dup).is_ok());
}
