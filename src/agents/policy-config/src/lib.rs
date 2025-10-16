use git2::{
    BranchType, Cred, Error, FetchOptions, RemoteCallbacks, Repository, StatusOptions, StatusShow,
};
use std::fs;
use std::path::Path;
use urlencoding::encode;

/// Generates an authenticated HTTPS URL for cloning, safely and idempotently.
pub fn get_auth_repo_url(repo_url: &str, username: &str, token: &str) -> String {
    let encoded_user = encode(username);
    let encoded_token = encode(token);

    // Normalize: if missing https://, assume it
    let normalized = if repo_url.starts_with("https://") {
        repo_url.to_string()
    } else if repo_url.starts_with("http://") {
        repo_url.replacen("http://", "https://", 1)
    } else if !repo_url.contains("://") {
        format!("https://{repo_url}")
    } else {
        repo_url.to_string()
    };

    // Already credentialed?
    if normalized.contains('@') {
        let creds = format!("{}:{}@", encoded_user, encoded_token);
        if normalized.contains(&creds) {
            return normalized;
        }
        return normalized; // assume already credentialed differently
    }

    // Inject credentials
    let rest = normalized.strip_prefix("https://").unwrap_or(&normalized);
    format!("https://{}:{}@{}", encoded_user, encoded_token, rest)
}

/// Clones the repository from the given URL to the specified path
pub fn clone_repo(
    repo_url: &str,
    repo_path: &str,
    username: &str,
    token: &str,
) -> Result<Repository, Error> {
    println!("Cloning repository...");
    Repository::clone(&get_auth_repo_url(repo_url, username, token), repo_path)
}

/// Checks if the repository has local uncommitted changes or untracked files
pub fn has_local_changes(repo: &Repository) -> Result<bool, Error> {
    let mut status_opts = StatusOptions::new();
    status_opts
        .include_untracked(true)
        .show(StatusShow::IndexAndWorkdir);
    let statuses = repo.statuses(Some(&mut status_opts))?;
    Ok(!statuses.is_empty()) // True if there are changes
}

/// Checks if the local branch has diverged from the remote
pub fn has_diverged_commits(repo: &Repository) -> Result<bool, Error> {
    let head = repo.head()?.resolve()?; // Get current HEAD
    let head_commit = head
        .target()
        .ok_or_else(|| Error::from_str("HEAD has no target"))?;

    let branch = repo.find_branch("main", BranchType::Local)?; // Assumes 'main' branch
    let binding = branch.upstream()?;
    let upstream = binding.get(); // Get remote tracking branch

    let upstream_commit = upstream
        .target()
        .ok_or_else(|| Error::from_str("No upstream target"))?;

    let base = repo.merge_base(head_commit, upstream_commit)?; // Find common ancestor

    // If HEAD and upstream have different commits, we have divergence
    Ok(head_commit != upstream_commit && head_commit != base)
}

/// Ensures the repository exists, is clean, and is up to date
pub fn get_latest_commit(
    repo_url: &str,
    repo_path: &str,
    username: &str,
    token: &str,
) -> Result<String, Error> {
    let repo_dir = Path::new(repo_path);

    // Set up authentication callbacks
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |_, _, _| Cred::userpass_plaintext(username, token));

    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(callbacks);

    // If the repository exists, check if it's clean; otherwise, delete and re-clone
    if repo_dir.exists() {
        let repo = Repository::open(repo_path)?;

        if has_local_changes(&repo)? || has_diverged_commits(&repo)? {
            println!("Repository is dirty (uncommitted changes, untracked files, or committed but unpushed changes). Re-cloning...");
            fs::remove_dir_all(repo_path).map_err(|e| Error::from_str(&e.to_string()))?;
            clone_repo(repo_url, repo_path, username, token)?;
        }
    } else {
        // Fresh clone if repo doesn't exist
        clone_repo(repo_url, repo_path, username, token)?;
    }

    // Open repository (clean or freshly cloned)
    let repo = Repository::open(repo_path)?;

    // Fetch latest updates
    {
        let mut remote = repo.find_remote("origin")?;
        remote.fetch(&["main"], Some(&mut fetch_options), None)?;
    }

    // Retrieve the latest commit hash
    let fetch_head = repo.refname_to_id("refs/remotes/origin/main")?;
    let commit = repo.find_commit(fetch_head)?;

    Ok(commit.id().to_string()) // Return latest commit hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Signature;
    use tempfile::TempDir;

    #[test]
    fn test_get_auth_repo_url_formats_correctly() {
        let url = "https://github.com/org/repo.git";
        let formatted = get_auth_repo_url(url, "user", "token123");
        assert_eq!(formatted, "https://user:token123@github.com/org/repo.git");
    }

    #[test]
    fn test_get_auth_repo_url_handles_missing_prefix() {
        let url = "github.com/org/repo.git";
        let formatted = get_auth_repo_url(url, "user", "token123");
        assert_eq!(formatted, "https://user:token123@github.com/org/repo.git");
    }

    #[test]
    fn test_has_local_changes_detects_changes() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        // create and commit an initial file
        let path = tmp.path().join("file.txt");
        fs::write(&path, "initial").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        let oid = index.write_tree().unwrap();
        let sig = Signature::now("Tester", "tester@example.com").unwrap();
        let tree = repo.find_tree(oid).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // modify file to create an uncommitted change
        fs::write(&path, "modified").unwrap();

        assert_eq!(has_local_changes(&repo).unwrap(), true);
    }

    #[test]
    fn test_has_local_changes_is_clean_after_commit() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        let path = tmp.path().join("clean.txt");
        fs::write(&path, "data").unwrap();

        let mut index = repo.index().unwrap();
        index.add_path(Path::new("clean.txt")).unwrap();
        let oid = index.write_tree().unwrap();
        let sig = Signature::now("Tester", "tester@example.com").unwrap();
        let tree = repo.find_tree(oid).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
            .unwrap();

        assert_eq!(has_local_changes(&repo).unwrap(), false);
    }

    #[test]
    fn test_has_diverged_commits_detects_divergence() -> anyhow::Result<()> {
        use git2::{Repository, Signature};
        use std::path::Path;
        use tempfile::TempDir;

        let tmp = TempDir::new()?;
        let repo = Repository::init(tmp.path())?;
        let sig = Signature::now("Tester", "tester@example.com")?;

        // --- Base commit ---
        let f = tmp.path().join("file.txt");
        std::fs::write(&f, "x")?;
        let mut idx = repo.index()?;
        idx.add_path(Path::new("file.txt"))?;
        let tree_id = idx.write_tree()?;
        let tree = repo.find_tree(tree_id)?;
        let c = repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])?;

        // Detach HEAD so we can create refs freely
        repo.set_head_detached(c)?;

        // Create both remote and local refs manually
        repo.reference("refs/remotes/origin/main", c, true, "simulate remote base")?;
        repo.reference("refs/heads/main", c, true, "create local main")?;

        // Reattach HEAD to main
        repo.set_head("refs/heads/main")?;

        // --- Define remote and tracking relationship ---
        repo.remote("origin", "https://example.com/dummy.git")?;
        {
            let mut cfg = repo.config()?;
            cfg.set_str("branch.main.remote", "origin")?;
            cfg.set_str("branch.main.merge", "refs/heads/main")?;
        }

        // --- Diverge locally ---
        for k in 0..2 {
            std::fs::write(&f, format!("commit {k}"))?;
            let mut idx = repo.index()?;
            idx.add_path(Path::new("file.txt"))?;
            let tid = idx.write_tree()?;
            let tree = repo.find_tree(tid)?;
            let parent = repo.head()?.peel_to_commit()?;
            repo.commit(
                Some("HEAD"),
                &sig,
                &sig,
                &format!("c{k}"),
                &tree,
                &[&parent],
            )?;
        }

        assert!(
            has_diverged_commits(&repo)?,
            "expected divergence between HEAD and origin/main"
        );

        Ok(())
    }

    #[test]
    fn test_idempotent_url_with_unicode_user() {
        let user = "ꨀ"; // non-ASCII edge case
        let token = "A-AAA";
        let domain = "github.com";
        let org = "A";
        let repo = "-";
        let raw = format!("https://{domain}/{org}/{repo}.git");

        let once = get_auth_repo_url(&raw, user, token);
        let twice = get_auth_repo_url(&once, user, token);

        let pattern = format!("{}:{}@{}:{}@", user, token, user, token);
        assert!(
            !twice.matches(&pattern).any(|_| true),
            "URL double-wrapped credentials: {twice}"
        );
    }

    #[test]
    fn test_divergence_with_branch_head_detached() {
        use git2::{Repository, Signature};
        use std::path::Path;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();
        let sig = Signature::now("Tester", "tester@example.com").unwrap();

        // Initial commit
        let f = tmp.path().join("file.txt");
        std::fs::write(&f, "x").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("file.txt")).unwrap();
        let tree_id = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let c = repo
            .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // Detach head, create tracking branch, and set up config
        repo.set_head_detached(c).unwrap();
        repo.reference("refs/remotes/origin/main", c, true, "simulate remote")
            .unwrap();
        repo.reference("refs/heads/main", c, true, "create local main")
            .unwrap();
        repo.set_head("refs/heads/main").unwrap();
        repo.remote("origin", "https://example.com/dummy.git")
            .unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("branch.main.remote", "origin").unwrap();
        cfg.set_str("branch.main.merge", "refs/heads/main").unwrap();

        // Diverge
        std::fs::write(&f, "commit 1").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("file.txt")).unwrap();
        let tid = idx.write_tree().unwrap();
        let tree = repo.find_tree(tid).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "c1", &tree, &[&parent])
            .unwrap();

        assert!(
            has_diverged_commits(&repo).unwrap(),
            "Expected divergence, but found none"
        );
    }

    #[test]
    fn test_clean_repo_invariant() {
        use git2::{Repository, Signature};
        use std::path::Path;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().to_path_buf(); // keep path alive
        let repo = Repository::init(&repo_path).unwrap();
        let sig = Signature::now("Tester", "test@example.com").unwrap();

        let f = repo_path.join("file.txt");
        std::fs::write(&f, "init").unwrap();

        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("file.txt")).unwrap();
        let tree_id = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // repo should be clean
        assert_eq!(
            has_local_changes(&repo).unwrap(),
            false,
            "Repo incorrectly detected local changes"
        );
    }

    #[test]
    fn test_detects_changes_after_modification() {
        use git2::{Repository, Signature};
        use std::path::Path;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();
        let sig = Signature::now("Tester", "test@example.com").unwrap();

        let f = tmp.path().join("file.txt");
        std::fs::write(&f, "data").unwrap();

        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("file.txt")).unwrap();
        let tree_id = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // Modify file
        std::fs::write(&f, "\u{1}\tȺ").unwrap();
        repo.index().unwrap().read(true).unwrap(); // refresh index

        assert!(
            has_local_changes(&repo).unwrap(),
            "Expected local changes after modifying tracked file"
        );
    }
}
