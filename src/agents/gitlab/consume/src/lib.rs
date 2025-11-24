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

use cassini_client::TcpClientMessage;
use neo4rs::{Config, ConfigBuilder};
use ractor::registry::where_is;
use std::error::Error;
use tracing::info;

pub mod groups;
pub mod meta;
pub mod pipelines;
pub mod projects;
pub mod repositories;
pub mod runners;
pub mod supervisor;
pub mod users;

pub const BROKER_CLIENT_NAME: &str = "GITLAB_CONSUMER_CLIENT";
pub const GITLAB_USER_CONSUMER: &str = "users";

pub struct GitlabConsumerState {
    pub registration_id: String,
    graph: neo4rs::Graph,
}
#[derive(Clone, Debug)]
pub struct GitlabConsumerArgs {
    pub registration_id: String,
    pub graph_config: neo4rs::Config,
}

///
/// Helper fn to setup consumer state, subscribe to a given topic, and connect to the graph database
pub async fn subscribe_to_topic(
    registration_id: String,
    topic: String,
    config: neo4rs::Config,
) -> Result<GitlabConsumerState, Box<dyn Error>> {
    let client = where_is(BROKER_CLIENT_NAME.to_string()).expect("Expected to find TCP client.");

    client.send_message(TcpClientMessage::Subscribe(topic))?;

    //load neo config and connect to graph db

    let graph = neo4rs::Graph::connect(config)?;

    Ok(GitlabConsumerState {
        registration_id,
        graph,
    })
}

#[derive(Debug)]
pub enum CrashReason {
    CaCertReadError(String), // error when reading CA cert
    SubscriptionError(String), // error when subscribing to the topic
                             // Add other error types as necessary
}
