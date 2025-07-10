/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

use cassini::{client::TcpClientMessage, ClientMessage};
use neo4rs::{Config, ConfigBuilder};
use ractor::registry::where_is;
use std::error::Error;
use tracing::info;

pub mod projects;
pub mod supervisor;

pub const BROKER_CLIENT_NAME: &str = "JIRA_CONSUMER_CLIENT";
pub const JIRA_USER_CONSUMER: &str = "users";

pub struct JiraConsumerState {
    pub registration_id: String,
    graph: neo4rs::Graph,
}
#[derive(Clone, Debug)]
pub struct JiraConsumerArgs {
    pub registration_id: String,
    pub graph_config: neo4rs::Config,
}

///
/// Helper fn to setup consumer state, subscribe to a given topic, and connect to the graph database
pub async fn subscribe_to_topic(
    registration_id: String,
    topic: String,
    config: neo4rs::Config,
) -> Result<JiraConsumerState, Box<dyn Error>> {
    let client = where_is(BROKER_CLIENT_NAME.to_string()).expect("Expected to find TCP client.");

    client.send_message(TcpClientMessage::Send(ClientMessage::SubscribeRequest {
        registration_id: Some(registration_id.clone()),
        topic,
    }))?;

    //load neo config and connect to graph db

    let graph = neo4rs::Graph::connect(config).await?;

    Ok(JiraConsumerState {
        registration_id,
        graph,
    })
}

pub fn get_neo_config() -> Config {
    let database_name = std::env::var("GRAPH_DB")
        .expect("Expected to get a neo4j database. GRAPH_DB variable not set.");
    let neo_user = std::env::var("GRAPH_USER").expect("No GRAPH_USER value set for Neo4J.");
    let neo_password =
        std::env::var("GRAPH_PASSWORD").expect("No GRAPH_PASSWORD provided for Neo4J.");
    let neo4j_endpoint = std::env::var("GRAPH_ENDPOINT").expect("No GRAPH_ENDPOINT provided.");
    info!("Using Neo4j database at {neo4j_endpoint}");

    let config = match std::env::var("GRAPH_CA_CERT") {
        Ok(client_certificate) => {
            info!("Found GRAPH_CA_CERT at {client_certificate}. Configuring graph client.");
            ConfigBuilder::default()
                .uri(neo4j_endpoint)
                .user(neo_user)
                .password(neo_password)
                .db(database_name)
                .fetch_size(500)
                .with_client_certificate(client_certificate)
                .max_connections(10)
                .build()
                .expect("Expected to build neo4rs configuration")
        }
        Err(_) => ConfigBuilder::default()
            .uri(neo4j_endpoint)
            .user(neo_user)
            .password(neo_password)
            .db(database_name)
            .fetch_size(500)
            .max_connections(10)
            .build()
            .expect("Expected to build neo4rs configuration"),
    };

    config
}

#[derive(Debug)]
pub enum CrashReason {
    CaCertReadError(String), // error when reading CA cert
    SubscriptionError(String), // error when subscribing to the topic
                             // Add other error types as necessary
}
