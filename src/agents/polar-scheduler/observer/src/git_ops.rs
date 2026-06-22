use git2::{FetchOptions, RemoteCallbacks, Repository};
use std::path::Path;
use tracing::info;

/// Clone a repository if it doesn't exist, otherwise open it.
/// Attempts public access first; if that fails and credentials are provided, retry with credentials.
pub fn ensure_repo(
    local_path: &Path,
    remote_url: &str,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<Repository, anyhow::Error> {
    if local_path.exists() {
        info!("Opening existing repo at {}", local_path.display());
        return Ok(Repository::open(local_path)?);
    }

    info!("Cloning {} into {}", remote_url, local_path.display());

    // Try without credentials first (public)
    let result = try_clone(remote_url, local_path, None);
    if result.is_ok() {
        return result;
    }

    // If we have credentials, try with them
    if let (Some(user), Some(pass)) = (username, password) {
        info!("Public clone failed, retrying with provided credentials");
        try_clone(remote_url, local_path, Some((user, pass)))
    } else {
        result
    }
}

fn try_clone(
    remote_url: &str,
    local_path: &Path,
    creds: Option<(&str, &str)>,
) -> Result<Repository, anyhow::Error> {
    let mut callbacks = RemoteCallbacks::new();
    if let Some((user, pass)) = creds {
        callbacks.credentials(move |_url, _username_from_url, _allowed| {
            git2::Cred::userpass_plaintext(user, pass)
        });
    }
    let mut fo = FetchOptions::new();
    fo.remote_callbacks(callbacks);
    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(fo);
    builder.clone(remote_url, local_path).map_err(Into::into)
}

/// Perform a `git pull` (fetch + fast-forward) on the repository.
/// Attempts public first, then with credentials if provided.
pub fn pull_repo(
    repo: &Repository,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<(), anyhow::Error> {
    info!("Pulling latest changes...");

    // Try without credentials first
    let result = try_pull(repo, None);
    if result.is_ok() {
        return result;
    }

    // If we have credentials, try with them
    if let (Some(user), Some(pass)) = (username, password) {
        info!("Public pull failed, retrying with provided credentials");
        try_pull(repo, Some((user, pass)))
    } else {
        result
    }
}

fn try_pull(repo: &Repository, creds: Option<(&str, &str)>) -> Result<(), anyhow::Error> {
    let mut remote = repo.find_remote("origin")?;
    let mut callbacks = RemoteCallbacks::new();
    if let Some((user, pass)) = creds {
        callbacks.credentials(move |_url, _username_from_url, _allowed| {
            git2::Cred::userpass_plaintext(user, pass)
        });
    }
    let mut fo = FetchOptions::new();
    fo.remote_callbacks(callbacks);
    remote.fetch(&[] as &[&str], Some(&mut fo), None)?;

    // Merge the fetched branch (origin/main) into current branch
    let fetch_head = repo.find_reference("FETCH_HEAD")?;
    let fetch_commit = repo.reference_to_annotated_commit(&fetch_head)?;
    let analysis = repo.merge_analysis(&[&fetch_commit])?;
    if analysis.0.is_up_to_date() {
        info!("Already up to date.");
    } else if analysis.0.is_fast_forward() {
        info!("Fast-forward possible. Performing merge...");
        let refname = format!("refs/heads/{}", repo.head()?.shorthand().unwrap_or("main"));
        repo.reference(
            &refname,
            fetch_commit.id(),
            true,
            "Fast-forward merge from origin",
        )?;
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))?;
    } else {
        info!("Merge required but not fast-forward. Skipping automatic merge.");
    }
    Ok(())
}
