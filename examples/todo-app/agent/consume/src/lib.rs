/*

pub mod actors;
pub const BROKER_CLIENT_NAME: &str = "TODO_CONSUMER_CLIENT";
use std::env;

use actors::TodoConsumerState;
use cassini::client::TcpClientMessage;
use cassini::ClientMessage;
use neo4rs::{Config, ConfigBuilder, Error};
use ractor::registry::where_is;

use utoipa::openapi::Deprecated;

pub fn get_neo_config() -> Config {
    let database_name = env::var("GRAPH_DB").unwrap();
    let neo_user = env::var("GRAPH_USER").unwrap();
    let neo_password = env::var("GRAPH_PASSWORD").unwrap();
    let neo4j_endpoint = env::var("GRAPH_ENDPOINT").unwrap();

    let config = ConfigBuilder::default() // Change from `new()` to `default()` if required
        .uri(neo4j_endpoint)
        .user(neo_user) // `.user(&str)` now takes ownership
        .password(neo_password) // `.password(&str)` now takes ownership
        .db(database_name) // `.db(&str)` now takes ownership
        .fetch_size(500)
        .max_connections(10)
        .build()
        .expect("Failed to build Neo4j configuration");

    config
}

/// Helper fn to setup consumer state, subscribe to a given topic, and connect to the graph database
pub async fn subscribe_to_topic(registration_id: String, topic: String) -> Result<TodoConsumerState, Error> {
    let client = where_is(BROKER_CLIENT_NAME.to_owned()).expect("Expected TCP client to be present.");
    client.send_message(TcpClientMessage::Send(ClientMessage::SubscribeRequest { registration_id: Some(registration_id.clone()), topic })).expect("Expected to send tcp client a message");

    //load neo config and connect to graph db
    match neo4rs::Graph::connect(get_neo_config()).await {
        Ok(graph) => Ok(TodoConsumerState { registration_id: registration_id, graph }),
         Err(e) => {
            Err(e)
        }
    }
}
*/
