//! The query the processor starts with, against a real Neo4j (the shared per-process fixture): what it
//! returns is what the processor wrote, and nothing it could not route.

use polar::graph::controller::handle_query;
use polar_scheduler::configuration::{STORED_DOCUMENTS_QUERY, documents_from_rows};
use polar_test_graph::{clear_graph, enter, neo4j};

#[tokio::test]
async fn the_documents_held_are_read_back_and_unroutable_ones_are_not() {
    let graph = neo4j();
    let _guard = enter();
    clear_graph(graph).await;

    // As handle_configuration writes them: a routed document, and one that could not be routed.
    graph
        .run(neo4rs::query(
            "CREATE (:Schedule:Configuration { id: 'one', target: 'agent-a', version: 3, \
             document: '{\"d\":1}', status: 'received' }), \
             (:Schedule:Configuration { id: 'unroutable:x.dhall', document: '{}', status: 'unroutable', \
             reason: 'no target' })",
        ))
        .await
        .unwrap();

    let rows = handle_query(
        graph,
        neo4rs::Query::new(STORED_DOCUMENTS_QUERY.to_string()),
    )
    .await
    .unwrap();
    let documents = documents_from_rows(&rows).unwrap();
    assert_eq!(documents.len(), 1, "{documents:?}");
    assert_eq!(documents[0].id, "one");
    assert_eq!(documents[0].agent, "agent-a");
    assert_eq!(documents[0].version, 3);
    assert_eq!(documents[0].document, "{\"d\":1}");
}
