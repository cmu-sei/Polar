//! The stored observation configuration, read from a real Neo4j (the shared per-process fixture).
//! Needs a container runtime, like the other graph tests.

use git_repo_scheduler::config::{REPO_CONFIG_QUERY, config_from_rows};
use polar::graph::controller::handle_query;
use polar::graph::nodes::git::RepoId;
use polar_test_graph::{clear_graph, enter, neo4j};

async fn read(repo_id: &RepoId) -> Option<git_agent_common::RepoObservationConfig> {
    let query = neo4rs::query(REPO_CONFIG_QUERY).param("repo_id", repo_id.to_string());
    let rows = handle_query(neo4j(), query).await.unwrap();
    config_from_rows(&rows).unwrap()
}

#[tokio::test]
async fn a_stored_configuration_is_returned_when_present_and_none_when_not() {
    let graph = neo4j();
    let _guard = enter();
    clear_graph(graph).await;
    let repo_id = RepoId::new("test-repo-123".into());
    let repo_url = "https://github.com/test/repo.git";

    assert!(read(&RepoId::new("nothing-here".into())).await.is_none());

    graph
        .run(
            neo4rs::query(
                r#"CREATE (c:RepoObservationConfig {
                    repo_id: $repo_id, repo_url: $repo_url, remotes: $remotes,
                    max_depth: $max_depth, tracked_refs: $tracked_refs })"#,
            )
            .param("repo_id", repo_id.to_string())
            .param("repo_url", repo_url)
            .param("remotes", vec!["origin".to_string()])
            .param("max_depth", 100_i64)
            .param("tracked_refs", vec!["refs/heads/main".to_string()]),
        )
        .await
        .unwrap();

    let fetched = read(&repo_id).await.expect("config should exist");
    assert_eq!(fetched.repo_id.to_string(), repo_id.to_string());
    assert_eq!(fetched.repo_url, repo_url);
    assert_eq!(fetched.remotes, ["origin"]);
    assert_eq!(fetched.refs, ["refs/heads/main"]);
    // Credentials are None until the credentials agent is wired in.
    assert!(fetched.credential.is_none());
}
