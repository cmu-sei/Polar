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

use polar_scheduler_observer::git_ops::{GitTls, ensure_repo, pull_repo};
use std::path::Path;
use tempfile::TempDir;

pub fn init_repo_with_commit(dir: &Path) -> git2::Repository {
    let repo = git2::Repository::init(dir).expect("failed to init repo");
    let sig = git2::Signature::now("test", "test@test.com").unwrap();
    let tree_id = {
        let mut index = repo.index().unwrap();
        index.write_tree().unwrap()
    };
    {
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    repo
}

#[test]
fn test_ensure_repo_opens_existing() {
    let dir = TempDir::new().unwrap();
    git2::Repository::init(dir.path()).expect("failed to init");
    let result = ensure_repo(dir.path(), "file:///nonexistent", &GitTls::default());
    assert!(result.is_ok(), "should open existing repo without cloning");
}

#[test]
fn test_ensure_repo_clones_when_missing() {
    let source = TempDir::new().unwrap();
    init_repo_with_commit(source.path());

    let dest = TempDir::new().unwrap();
    let dest_path = dest.path().join("cloned");

    let remote_url = format!("file://{}", source.path().display());
    let result = ensure_repo(&dest_path, &remote_url, &GitTls::default());
    assert!(
        result.is_ok(),
        "should clone from file:// remote: {result:?}"
    );
    assert!(dest_path.join(".git").exists());
}

#[test]
fn test_ensure_repo_fails_bad_remote() {
    let dest = TempDir::new().unwrap();
    let dest_path = dest.path().join("cloned");
    let result = ensure_repo(&dest_path, "file:///nonexistent/repo", &GitTls::default());
    assert!(result.is_err(), "should fail with bad remote");
}

#[test]
fn test_pull_repo_up_to_date() {
    let source = TempDir::new().unwrap();
    init_repo_with_commit(source.path());

    let dest = TempDir::new().unwrap();
    let dest_path = dest.path().join("cloned");
    let remote_url = format!("file://{}", source.path().display());

    ensure_repo(&dest_path, &remote_url, &GitTls::default()).unwrap();
    let result = pull_repo(&dest_path, &GitTls::default());
    assert!(
        result.is_ok(),
        "pull on up-to-date repo should succeed: {result:?}"
    );
}

#[test]
fn test_pull_repo_brings_in_a_new_commit() {
    let source = TempDir::new().unwrap();
    let repo = init_repo_with_commit(source.path());

    let dest = TempDir::new().unwrap();
    let dest_path = dest.path().join("cloned");
    let remote_url = format!("file://{}", source.path().display());
    ensure_repo(&dest_path, &remote_url, &GitTls::default()).unwrap();

    std::fs::write(source.path().join("new.dhall"), "1").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("new.dhall")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("test", "test@test.com").unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&parent])
        .unwrap();

    pull_repo(&dest_path, &GitTls::default()).unwrap();
    assert!(dest_path.join("new.dhall").exists());
}
