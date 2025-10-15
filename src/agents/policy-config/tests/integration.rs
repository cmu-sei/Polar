// tests/integration.rs
use std::env;

#[test]
#[ignore] // use `cargo test -- --ignored` to run manually
fn test_get_latest_commit_real_repo() {
    let repo_url = "https://github.com/cmu-sei/Polar.git";
    let repo_path = "/tmp/polar_clone_test";
    let username = env::var("GITHUB_USER").expect("set GITHUB_USER");
    let token = env::var("GITHUB_TOKEN").expect("set GITHUB_TOKEN");

    let commit = get_latest_commit(&repo_url, repo_path, &username, &token)
        .expect("failed to get latest commit");
    assert_eq!(commit.len(), 40); // SHA length
}
