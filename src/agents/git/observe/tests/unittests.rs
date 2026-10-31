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
        None,
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
        None,
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
        None,
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
        None,
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
/// Takes every event but those of one ref past the first, as a client that has no room left would refuse them.
struct RefusingSink {
    refuse_on: &'static str,
    accepted: std::sync::Mutex<Vec<String>>,
}

impl EventSink for RefusingSink {
    async fn publish(&self, event: &git_agent_common::GitRepositoryMessage) -> Result<(), String> {
        let ref_name = match event {
            git_agent_common::GitRepositoryMessage::CommitDiscovered { ref_name, .. }
            | git_agent_common::GitRepositoryMessage::RefUpdated { ref_name, .. } => {
                ref_name.clone()
            }
        };
        let mut accepted = self.accepted.lock().unwrap();
        if ref_name == self.refuse_on && accepted.contains(&ref_name) {
            return Err("the client's outbox and buffer are full".to_string());
        }
        accepted.push(ref_name);
        Ok(())
    }
}

/// A ref with an event the client did not accept keeps the checkpoint it had, so its commits are walked, and
/// published, again; a ref all of whose events were accepted moves on. Until 2026-10-30 a refused publish was logged
/// and dropped inside the walk, and the ref moved past it: the commit was lost.
#[tokio::test]
async fn an_event_the_client_refused_leaves_its_ref_s_last_seen_where_it_was() {
    init_logging();
    let (_dir, repo) = bare_repo();
    let main_commits = linear_history(&repo, "refs/heads/main", 2);
    let dev_commits = linear_history(&repo, "refs/heads/dev", 3);
    let repo_id = RepoId::from_url("local");
    let sink = RefusingSink {
        refuse_on: "refs/heads/dev",
        accepted: Default::default(),
    };
    let mut main = Checkpoint::default();
    let mut dev = Checkpoint::default();
    assert!(
        publish_ref(&sink, &repo, &repo_id, "refs/heads/main", &mut main, 100, 1)
            .await
            .unwrap()
    );
    assert!(
        !publish_ref(&sink, &repo, &repo_id, "refs/heads/dev", &mut dev, 100, 1)
            .await
            .unwrap()
    );
    assert_eq!(main.tip.as_ref(), main_commits.last());
    assert_eq!(
        dev.tip,
        None,
        "dev's second event was refused: dev stays where it was ({:?})",
        dev_commits.last()
    );
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

/// Takes events until it has taken `room`, then refuses them: a client whose room ran out.
struct RoomFor {
    room: usize,
    /// What it took: a commit's oid, or none for a ref's update.
    taken: std::sync::Mutex<Vec<Option<String>>>,
}

impl RoomFor {
    fn new(room: usize) -> Self {
        Self {
            room,
            taken: Default::default(),
        }
    }

    fn commits(&self) -> Vec<String> {
        self.taken
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .cloned()
            .collect()
    }

    fn ref_updates(&self) -> usize {
        self.taken
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.is_none())
            .count()
    }
}

impl EventSink for RoomFor {
    async fn publish(&self, event: &git_agent_common::GitRepositoryMessage) -> Result<(), String> {
        let mut taken = self.taken.lock().unwrap();
        if taken.len() >= self.room {
            return Err("the client's outbox and buffer are full".to_string());
        }
        taken.push(match event {
            git_agent_common::GitRepositoryMessage::CommitDiscovered { oid, .. } => {
                Some(oid.clone())
            }
            git_agent_common::GitRepositoryMessage::RefUpdated { .. } => None,
        });
        Ok(())
    }
}

/// Builds a history from `parents`: commit `i` has the commits `parents[i]` (all earlier) as its parents; the ref
/// points at the last. Returns every commit.
fn history(repo: &Repository, ref_name: &str, parents: &[Vec<usize>]) -> Vec<Oid> {
    let mut oids: Vec<Oid> = Vec::new();
    for (i, of) in parents.iter().enumerate() {
        let commits: Vec<Commit> = of
            .iter()
            .map(|p| repo.find_commit(oids[*p]).unwrap())
            .collect();
        let oid = make_commit(repo, &format!("c{i}"), &commits.iter().collect::<Vec<_>>());
        oids.push(oid);
    }
    repo.reference(ref_name, *oids.last().unwrap(), true, "init")
        .unwrap();
    oids
}

/// The commits reachable from `tip`.
fn reachable(repo: &Repository, tip: Oid) -> std::collections::BTreeSet<String> {
    let mut walk = repo.revwalk().unwrap();
    walk.push(tip).unwrap();
    walk.map(|oid| oid.unwrap().to_string()).collect()
}

/// Every commit of `order` comes after those of its parents that are in it.
fn parents_first(repo: &Repository, order: &[String]) -> bool {
    order.iter().enumerate().all(|(i, oid)| {
        let commit = repo.find_commit(Oid::from_str(oid).unwrap()).unwrap();
        commit.parent_ids().all(|parent| {
            let parent = parent.to_string();
            order
                .iter()
                .position(|o| *o == parent)
                .is_none_or(|p| p < i)
        })
    })
}

/// A walk larger than one batch, cut after its first accepted batch, resumes with nothing lost and nothing
/// published twice, merges included: commits go oldest first, and the checkpoint moves after each accepted batch
/// (cassini-backpressure.md, the decisions reviewed with the maintainer, 17).
#[tokio::test]
async fn a_walk_larger_than_a_batch_cut_after_its_first_batch_resumes_with_nothing_lost_or_duplicated()
 {
    init_logging();
    let (_dir, repo) = bare_repo();
    // root; a branch of four and one of three from it, merged; three more; a merge of a branch of two.
    let parents = vec![
        vec![],
        vec![0],
        vec![1],
        vec![2],
        vec![3],
        vec![0],
        vec![5],
        vec![6],
        vec![4, 7],
        vec![8],
        vec![9],
        vec![8],
        vec![10, 11],
    ];
    let oids = history(&repo, "refs/heads/main", &parents);
    let repo_id = RepoId::from_url("local");
    let mut checkpoint = Checkpoint::default();
    let first = RoomFor::new(4);
    let done = publish_ref(
        &first,
        &repo,
        &repo_id,
        "refs/heads/main",
        &mut checkpoint,
        100,
        4,
    )
    .await
    .unwrap();
    assert!(!done, "the second batch was refused");
    assert_eq!(first.commits().len(), 4, "one batch was taken");
    let rest = RoomFor::new(usize::MAX);
    let done = publish_ref(
        &rest,
        &repo,
        &repo_id,
        "refs/heads/main",
        &mut checkpoint,
        100,
        4,
    )
    .await
    .unwrap();
    assert!(done);
    let mut order = first.commits();
    order.extend(rest.commits());
    let published: std::collections::BTreeSet<String> = order.iter().cloned().collect();
    assert_eq!(
        published.len(),
        order.len(),
        "nothing published twice: {order:?}"
    );
    assert_eq!(
        published,
        reachable(&repo, *oids.last().unwrap()),
        "nothing lost"
    );
    assert!(parents_first(&repo, &order), "oldest first");
    assert_eq!(
        rest.ref_updates(),
        1,
        "the ref's update once, after its commits"
    );
    assert_eq!(checkpoint.tip, oids.last().copied());
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(48))]
    /// Over any history, any batch size and a cut after any number of accepted batches, the two walks together
    /// publish every commit once, each after its parents.
    #[test]
    fn any_history_cut_after_any_batch_resumes_with_nothing_lost_or_duplicated(
        shape in proptest::collection::vec((0usize..1000, 0usize..1000, proptest::bool::ANY), 1..16),
        batch in 1usize..6,
        batches_taken in 0usize..6,
    ) {
        // Commit i > 0 has one or two parents among those before it.
        let parents: Vec<Vec<usize>> = std::iter::once(Vec::new())
            .chain(shape.iter().enumerate().map(|(i, (a, b, two))| {
                let i = i + 1;
                let first = a % i;
                let second = b % i;
                if *two && second != first { vec![first, second] } else { vec![first] }
            }))
            .collect();
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let (_dir, repo) = bare_repo();
        let oids = history(&repo, "refs/heads/main", &parents);
        let repo_id = RepoId::from_url("local");
        let mut checkpoint = Checkpoint::default();
        let first = RoomFor::new(batch * batches_taken);
        runtime
            .block_on(publish_ref(&first, &repo, &repo_id, "refs/heads/main", &mut checkpoint, 1000, batch))
            .unwrap();
        let rest = RoomFor::new(usize::MAX);
        let done = runtime
            .block_on(publish_ref(&rest, &repo, &repo_id, "refs/heads/main", &mut checkpoint, 1000, batch))
            .unwrap();
        proptest::prop_assert!(done);
        let mut order = first.commits();
        order.extend(rest.commits());
        let published: std::collections::BTreeSet<String> = order.iter().cloned().collect();
        proptest::prop_assert_eq!(published.len(), order.len());
        proptest::prop_assert_eq!(published, reachable(&repo, *oids.last().unwrap()));
        proptest::prop_assert!(parents_first(&repo, &order));
    }
}
