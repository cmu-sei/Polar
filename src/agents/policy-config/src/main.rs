use policy_config::get_latest_commit;

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
