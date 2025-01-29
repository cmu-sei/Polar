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


use std::error::Error;

use cassini::{client::TcpClientMessage, ClientMessage};
//TODO: Move to global consumer common lib
use common::{read_from_env, types::GitlabData};
use tracing::{debug, error, info};
use neo4rs::{Config, ConfigBuilder, Query, Txn};
use ractor::{registry::where_is, ActorProcessingErr, MessagingErr};
use url::Url;


pub mod supervisor;
pub mod users;
//TODO: Uncomment when other actors are done
// pub mod projects;
// pub mod groups;
// pub mod runners;

pub const BROKER_CLIENT_NAME: &str = "GITLAB_CONSUMER_CLIENT";
pub const GITLAB_USER_CONSUMER: &str = "users";
//TODO: Give consumer state info about neo4j, and eventually, a graph adapter to work with
pub struct GitlabConsumerState {
    pub registration_id: String,
    graph: neo4rs::Graph
}
#[derive(Clone, Debug)]
pub struct GitlabConsumerArgs {
    pub registration_id: String
}

///
/// Helper fn to setup consumer state, subscribe to a given topic, and connect to the graph database
/// TODO: Consider updating this function in the future to leverage a grpah adapter should support alternatives to neo4j
pub async fn subscribe_to_topic(registration_id: String, topic: String) -> Result<GitlabConsumerState, Box<dyn Error>> {
    match where_is(BROKER_CLIENT_NAME.to_string()) {
        Some(client) => {
            if let Err(e) = client.send_message(TcpClientMessage::Send(ClientMessage::SubscribeRequest { registration_id: Some(registration_id.clone()), topic })) {
                return Err(e.into())
            }
            //load neo config and connect to graph db
            match neo4rs::Graph::connect(get_neo_config()).await {
                Ok(graph) => Ok(GitlabConsumerState { registration_id: registration_id, graph }),
                 Err(e) => {
                    Err(e.into())
                }
            }
        }
        None =>  {
            error!("Couldn't locate tcp client!");
            Err(ActorProcessingErr::from("Couldn't locate tcp client!".to_string()))
        }
    }
}

pub async fn run_query(txn: &Txn, query: String) -> bool {
    match txn.run(Query::new(query.clone())).await {
        Ok(_) => {
            debug!("{}", query);
            return true
        }, 
        Err(e) => {
            error!("Could not execute query! {}", e);
            return false
        }
    }
}
pub fn get_neo4j_endpoint() -> String {
    let endpoint = read_from_env("GRAPH_ENDPOINT".to_owned());
    match Url::parse(endpoint.as_str()) {

        Ok(url) => return url.to_string(),

        Err(e) => panic!("error: {}, the  provided neo4j endpoint is not valid.", e)
    }
}

pub fn get_neo_config() -> Config {
    let database_name = read_from_env("GRAPH_DB".to_owned());
    let neo_user = read_from_env("GRAPH_USER".to_owned());
    let neo_password = read_from_env("GRAPH_PASSWORD".to_owned());
    
    let config = ConfigBuilder::new()
    .uri(get_neo4j_endpoint())
    .user(&neo_user)
    .password(&neo_password)
    .db(&database_name)
    .fetch_size(500).max_connections(10).build().unwrap();
    
    return config;
}

