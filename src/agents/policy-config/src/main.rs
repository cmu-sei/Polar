use git2::{
    BranchType, Cred, Error, FetchOptions, RemoteCallbacks, Repository, StatusOptions, StatusShow,
};
use std::fs;
use std::path::Path;

/// Generates an authenticated URL for cloning
fn get_auth_repo_url(repo_url: &str, username: &str, token: &str) -> String {
    format!(
        "https://{}:{}@{}",
        username,
        token,
        repo_url.strip_prefix("https://").unwrap_or(repo_url)
    )
}

/// Clones the repository from the given URL to the specified path
fn clone_repo(
    repo_url: &str,
    repo_path: &str,
    username: &str,
    token: &str,
) -> Result<Repository, Error> {
    println!("Cloning repository...");
    Repository::clone(&get_auth_repo_url(repo_url, username, token), repo_path)
}

/// Checks if the repository has local uncommitted changes or untracked files
fn has_local_changes(repo: &Repository) -> Result<bool, Error> {
    let mut status_opts = StatusOptions::new();
    status_opts
        .include_untracked(true)
        .show(StatusShow::IndexAndWorkdir);
    let statuses = repo.statuses(Some(&mut status_opts))?;
    Ok(!statuses.is_empty()) // True if there are changes
}

/// Checks if the local branch has diverged from the remote
fn has_diverged_commits(repo: &Repository) -> Result<bool, Error> {
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
fn get_latest_commit(
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

fn main() {
    let repo_url = "https://github.com/cmu-sei/Polar.git"; // Change for GitLab if needed
    let repo_path = "/tmp/my_repo"; // Local directory
    let username = "daveman1010221"; // Your GitHub/GitLab username
    let token = "ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"; // OAuth or Personal Access Token

    match get_latest_commit(repo_url, repo_path, username, token) {
        Ok(commit_hash) => println!("Latest commit hash: {}", commit_hash),
        Err(e) => eprintln!("Error fetching commit: {}", e),
    }
}
