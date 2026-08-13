use polar_scheduler_observer::git_ops::{ensure_repo, pull_repo};
use std::path::Path;
use tempfile::TempDir;


fn init_repo_with_commit(dir: &Path) -> git2::Repository {
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
    let result = ensure_repo(dir.path(), "file:///nonexistent", None, None);
    assert!(result.is_ok(), "should open existing repo without cloning");
}

#[test]
fn test_ensure_repo_clones_when_missing() {
    let source = TempDir::new().unwrap();
    init_repo_with_commit(source.path());

    let dest = TempDir::new().unwrap();
    let dest_path = dest.path().join("cloned");

    let remote_url = format!("file://{}", source.path().display());
    let result = ensure_repo(&dest_path, &remote_url, None, None);
    assert!(result.is_ok(), "should clone from file:// remote");
    assert!(dest_path.exists());
}

#[test]
fn test_ensure_repo_fails_bad_remote() {
    let dest = TempDir::new().unwrap();
    let dest_path = dest.path().join("cloned");
    let result = ensure_repo(&dest_path, "file:///nonexistent/repo", None, None);
    assert!(result.is_err(), "should fail with bad remote");
}

#[test]
fn test_pull_repo_up_to_date() {
    let source = TempDir::new().unwrap();
    init_repo_with_commit(source.path());

    let dest = TempDir::new().unwrap();
    let dest_path = dest.path().join("cloned");
    let remote_url = format!("file://{}", source.path().display());

    ensure_repo(&dest_path, &remote_url, None, None).unwrap();
    let repo = git2::Repository::open(&dest_path).unwrap();
    let result = pull_repo(&repo, None, None);
    assert!(result.is_ok(), "pull on up-to-date repo should succeed");
}
