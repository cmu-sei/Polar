//! The stored observation configuration, read through the graph controller from a real Neo4j.
//! Needs a container runtime (testcontainers), like the other graph tests.

use git_repo_scheduler::config::fetch_repo_observation_config;
use polar::graph::controller::{GraphControllerActor, GraphControllerArgs};
use polar::graph::environment::GraphSettings;
use polar::graph::nodes::git::RepoId;
use ractor::Actor;
use std::collections::HashMap;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::neo4j::Neo4j;

async fn controller_on_neo4j() -> (
    testcontainers::ContainerAsync<testcontainers_modules::neo4j::Neo4jImage>,
    ractor::ActorRef<polar::graph::controller::GraphControllerMsg>,
    neo4rs::Graph,
) {
    let container = Neo4j::default().start().await.unwrap();
    let uri = format!(
        "bolt://{}:{}",
        container.get_host().await.unwrap(),
        container.image().bolt_port_ipv4().unwrap()
    );
    let user = container.image().user().expect("default user").to_string();
    let password = container
        .image()
        .password()
        .expect("default password")
        .to_string();

    let env: HashMap<String, String> = [
        ("GRAPH_DB", "neo4j"),
        ("GRAPH_USER", user.as_str()),
        ("GRAPH_PASSWORD", password.as_str()),
        ("GRAPH_ENDPOINT", uri.as_str()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let settings = GraphSettings::from_env(&env).unwrap();
    let (controller, _) = Actor::spawn(
        None,
        GraphControllerActor,
        GraphControllerArgs::new(std::sync::Arc::new(ractor::OutputPort::default()))
            .with_settings(settings),
    )
    .await
    .unwrap();

    let graph = neo4rs::Graph::connect(
        neo4rs::ConfigBuilder::new()
            .uri(uri)
            .user(user)
            .password(password)
            .build()
            .unwrap(),
    )
    .unwrap();
    (container, controller, graph)
}

#[tokio::test]
async fn a_stored_configuration_is_returned_when_present() {
    let (_container, controller, graph) = controller_on_neo4j().await;
    let repo_id = RepoId::new("test-repo-123".into());
    let repo_url = "https://github.com/test/repo.git";

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

    let fetched = fetch_repo_observation_config(&controller, &repo_id)
        .await
        .unwrap()
        .expect("config should exist");
    assert_eq!(fetched.repo_id.to_string(), repo_id.to_string());
    assert_eq!(fetched.repo_url, repo_url);
    assert_eq!(fetched.remotes, ["origin"]);
    assert_eq!(fetched.refs, ["refs/heads/main"]);
    // Credentials are None until the credentials agent is wired in.
    assert!(fetched.credentials.is_none());
}

#[tokio::test]
async fn no_configuration_is_none() {
    let (_container, controller, _graph) = controller_on_neo4j().await;
    let fetched = fetch_repo_observation_config(&controller, &RepoId::new("nothing-here".into()))
        .await
        .unwrap();
    assert!(fetched.is_none());
}
