// tests/integration.rs
use policy_config::*;
use std::env;

/// Integration test: fetches the latest commit SHA from the live Polar repository on GitHub.
///
/// # Purpose
/// This test verifies that [`get_latest_commit`] can:
/// - Authenticate to a real remote repository using a GitHub Personal Access Token (PAT)
/// - Clone or fetch the repository into a local path
/// - Retrieve and return the latest commit SHA as a 40-character hexadecimal string
///
/// # Requirements
/// - Network connectivity (HTTPS access to GitHub)
/// - Valid GitHub credentials provided via environment variables:
///   - `GITHUB_USER` — your GitHub username
///   - `GITHUB_TOKEN` — a **fine-grained Personal Access Token (PAT)** with at least `repo:read` scope
/// - A writable local path (`/tmp/polar_clone_test` by default)
///
/// # Usage
/// This test is marked `#[ignore]` to avoid running by default in CI or offline environments.
/// To run it manually:
///
/// ```bash
/// export GITHUB_USER="your-username"
/// export GITHUB_TOKEN="your-token"
/// cargo test --test integration -- --ignored --nocapture
/// ```
///
/// The test will clone or update the repository at `/tmp/polar_clone_test`
/// and assert that the retrieved commit hash is 40 characters long.
///
/// # Notes
/// - Running this test repeatedly will re-use the local clone path.
/// - If you encounter rate limiting or auth errors, regenerate your GitHub token.
/// - Consider mocking `get_latest_commit()` in CI to avoid external dependencies.
///
/// # Example output
/// ```text
/// running 1 test
/// test test_get_latest_commit_real_repo ... ok
/// ```
#[test]
#[ignore] // use `cargo test -- --ignored` to run manually
fn test_get_latest_commit_real_repo() {
    let repo_url = "https://github.com/cmu-sei/Polar.git";
    let repo_path = "/tmp/polar_clone_test";
    let username = env::var("GITHUB_USER").expect("set GITHUB_USER");
    let token = env::var("GITHUB_TOKEN").expect("set GITHUB_TOKEN");

    let commit = get_latest_commit(repo_url, repo_path, &username, &token)
        .expect("failed to get latest commit");
    assert_eq!(commit.len(), 40); // SHA length
}
