pub mod actors;

pub const BROKER_CLIENT_NAME: &str = "polar.web.tcp.client";

use actors::ApiConsumerState;
use cassini_client::TcpClientMessage;
use neo4rs::Error;
use polar::get_neo_config;
use ractor::registry::where_is;

/// Helper fn to setup consumer state, subscribe to a given topic, and connect to the graph database
pub async fn subscribe_to_topic(
    registration_id: String,
    topic: String,
) -> Result<ApiConsumerState, Error> {
    let client =
        where_is(BROKER_CLIENT_NAME.to_owned()).expect("Expected TCP client to be present.");
    client
        .send_message(TcpClientMessage::Subscribe(topic.clone()))
        .expect("Expected to send tcp client a message");

    let config = get_neo_config().unwrap();

    //load neo config and connect to graph db
    match neo4rs::Graph::connect(config) {
        Ok(graph) => Ok(ApiConsumerState {
            registration_id: registration_id,
            graph,
        }),
        Err(e) => Err(e),
    }
}
