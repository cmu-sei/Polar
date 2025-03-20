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
use gitlab_queries::{groups::GroupData, Namespace, Project};
use tracing::{debug, error, info};
use neo4rs::{Config, ConfigBuilder, Query, Txn};
use ractor::{registry::where_is, ActorProcessingErr, MessagingErr};
use url::Url;

pub mod supervisor;
pub mod users;
pub mod projects;
pub mod groups;
pub mod runners;

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

pub fn get_neo_config() -> Config {
    let database_name = std::env::var("GRAPH_DB").expect("Expected to get a neo4j database. GRAPH_DB variable not set.");
    let neo_user = std::env::var("GRAPH_USER").expect("No GRAPH_USER value set for Neo4J.");
    let neo_password = std::env::var("GRAPH_PASSWORD").expect("No GRAPH_PASSWORD provided for Neo4J.");
    let neo4j_endpoint = std::env::var("GRAPH_ENDPOINT").expect("No GRAPH_ENDPOINT provided.");

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
/// Helper function to create group nodes
// pub fn merge_group_query(group: Group) -> String {
//     format!(r#"
//         MERGE (group: GitlabGroupGitlabGroup {{
//             group_id: "{group_id}",
//             full_name: "{full_name}",
//             full_path: "{group_full_path}",
//             created_at: "{group_created_at}",
//             member_count: "{group_members_count}"
//         }})
//     "#,
//     group_id = group.id, 
//     full_name = group.full_name, 
//     group_full_path = group.full_path,
//     group_created_at = group.created_at.unwrap_or_default(), 
//     group_members_count = group.group_members_count,
//     )
// }

pub fn merge_namespace_query(namespace: Namespace) -> String {
    
    format!(r#"
    MERGE (namespace: GitlabNamespace {{
        namespace_id: "{namespace_id}",
        full_name: "{full_name}",
        full_path: "{full_path}"
    }})
    "#,
    namespace_id = namespace.id,
    full_name = namespace.full_name,
    full_path = namespace.full_path,

    )
}


//TODO: Helper function to create project nodes and their relationships
// pub fn merge_project_query(project: Project) -> String {
//     let merge_group_query = match project.group {
//         //connect group if presenet
//         Some(group) =>  { 
//          //if namespace exists, compose a query to create a node for it and a draw a relationship
//          format!( "{0}\n{1}", merge_group_query(group), "WITH project, group MERGE (project)-[:inGroup]->(group);")    
//         }
//         None => String::default()
//     };

//     //NOTE: Not entirely sure its needed to match here, because all projects exist in either a user or group namespace
//     //better safe than sorry
//     let merge_namespace_query = match project.namespace {
//         //connect group if presenet
//         Some(namespace) =>  {
//             //if namespace exists, compose a query to create a node for it and a draw a relationship
//             format!( "{0}\n{1}", merge_namespace_query(namespace), "WITH project, namespace MERGE (project)-[:inNamespace]->(namespace)")
//         },
//         None => String::default()
//     };

//     format!(
//         r#"
//             MERGE (project:GitlabProject {{ 
//                 project_id: "{project_id}"
//             }})
//             SET project.name = "{name}",
//                 project.full_path = "{full_path}",
//                 project.created_at = "{created_at}",
//                 project.last_activity_at = "{last_activity_at}"
//             {merge_namespace_query}
//             {merge_group_query}   
//         "#, 
//         project_id = project.id,
//         name = project.name,
//         full_path = project.full_path,
//         created_at = project.created_at.unwrap_or_default(),
//         last_activity_at = project.last_activity_at.unwrap_or_default(),
//     )
// }
