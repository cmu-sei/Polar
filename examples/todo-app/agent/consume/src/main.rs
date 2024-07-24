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

use common::{connect_to_rabbitmq};
use serde_json::json;

use futures_lite::StreamExt;
use todo_types::{Todo, MessageType, TODO_EXCHANGE_STR, TODO_QUEUE_NAME};
use lapin::{options::{BasicAckOptions, QueueBindOptions, BasicConsumeOptions}, Result, types::FieldTable};
use neo4rs::Query;
use std::env;
use neo4rs::{Config, ConfigBuilder};
use url::Url;

pub fn get_neo4j_endpoint() -> String {
    let endpoint = env::var("GRAPH_ENDPOINT").expect("Could not load graph instance endpoint from environment.");
    match Url::parse(endpoint.as_str()) {

       Ok(url) => return url.to_string(),

       Err(e) => panic!("error: {}, the  provided neo4j endpoint is not valid.", e)
    }
}

pub fn get_neo_config() -> Config {
    let database_name = env::var("GRAPH_DB").unwrap();
    let neo_user = env::var("GRAPH_USER").unwrap();
    let neo_password = env::var("GRAPH_PASSWORD").unwrap();
    
    let config = ConfigBuilder::new()
    .uri(get_neo4j_endpoint())
    .user(&neo_user)
    .password(&neo_password)
    .db(&database_name)
    .fetch_size(500).max_connections(10).build().unwrap();
    
    return config;
}

#[tokio::main]
async fn main() -> Result<()> {
    //get mq connection
    let conn = connect_to_rabbitmq().await.unwrap();

    //create channels, exchange, 
    let consumer_channel = conn.create_channel().await?;

    //bind to queue
    consumer_channel.queue_bind(TODO_QUEUE_NAME, TODO_EXCHANGE_STR, "", QueueBindOptions::default(), FieldTable::default()).await?;

    println!("[*] waiting to consume");
    let mut consumer = consumer_channel
    .basic_consume(
        TODO_QUEUE_NAME,
        "todo_consumer",
        BasicConsumeOptions::default(),
        FieldTable::default(),
    )
    .await?;

    //load neo config and connect to graph db TODO: get credentials securely
    let graph_conn = neo4rs::Graph::connect(get_neo_config()).await.unwrap();
    println!("[*] Connected to neo4j");
    //begin consume loop

    while let Some(delivery) = consumer.next().await {
        let delivery = delivery.expect("error in consumer");
        delivery
            .ack(BasicAckOptions::default())
            .await
            .expect("ack");

        // //deserialize json value
        let data_str = String::from_utf8(delivery.data).unwrap();
       // println!("received {}", data_str);
        
        let message : MessageType = serde_json::from_str(data_str.as_str()).unwrap();

        let transaction = graph_conn.start_txn().await.unwrap();

        match message {
            MessageType::Todo(vec) => {
                for todo in vec as Vec<Todo> {
                    let query = format!("CREATE (n: Todo {{id: \"{}\", title: \"{}\", completed: \"{}\" }}) return n ", todo.id, todo.title, todo.completed);
                    println!("{}", query);

                    transaction.run(Query::new(query)).await.expect("Could not execute query on neo4j graph");
                }
            },
            //TODO: Implement putting the api spec within the graph, represent each endpoint as a node?
            MessageType::OpenApiSpec(spec) => {
                //decompose the api spec, create a node for the application itself, and nodes for each endpoint, which should have relationships to their operations.
                let mut query = format!(
                    "CREATE (o:Application {{ \
                    openapi_version: \"{}\", \
                    title: \"{}\", \
                    description: \"{}\", \
                    version: \"{}\", \
                    license: \"{}\" \
                }}) RETURN o",
                serde_json::json!(spec.openapi).as_str().unwrap_or_default(),
                spec.info.title,
                spec.info.description.unwrap_or_default(),
                spec.info.version,
                spec.info.license.unwrap_or_default().name
                );
                println!("{}", query);

                //transaction.run(Query::new(query)).await.expect("Could not execute query on neo4j graph");

                //iterate through paths
                for (endpoint, path_item) in spec.paths.paths.iter() {
                    println!("found endpoint \"{endpoint}\"");
                    //create node for endpoint

                    query = format!(
                        "CREATE (p:Path {{ \
                            summary: '{}', \
                            description: '{}' \
                        }}) RETURN p",
                        path_item.summary.clone().unwrap_or_default(),
                        path_item.description.clone().unwrap_or_default(),
                    );
                    println!("{}", query);
                   // transaction.run(Query::new(query)).await.expect("Could not execute query on neo4j graph");
                    
                    for (operation_type, operation) in path_item.operations.iter() {
                        println!("found operation of type with id {}", operation.operation_id.clone().unwrap_or_default());

                        //TODO: process
                        //create node for endpoint operation
                        let newquery = format!(
                        "CREATE (o:EndpointOperation {{ \
                            operation_id: '{}', \
                            description: '{}' \
                        }}) RETURN o",
                        operation.operation_id.clone().unwrap_or_default(),
                        operation.description.clone().unwrap_or_default(),
                        );
                        println!("{}", newquery);
                    }
                }
            }
            _ => todo!()
        }
        match transaction.commit().await {
            Ok(_) => {
                println!("[*] Transaction Committed")
             },
             Err(e) => panic!("Error updating graph {}", e)
        }

    } //end consume loop

    Ok(())
}
