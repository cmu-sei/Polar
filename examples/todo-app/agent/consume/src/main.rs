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
   distribution.  Please see Copyright notice for non-US Government use and distribution.2

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/
use env_logger::{Builder, Env};
use log::{info, debug, trace};
use common::{connect_to_rabbitmq};
use utoipa::openapi::{PathItemType,Deprecated};
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

pub fn get_operation_str(path_item_type: &PathItemType) -> &str {
    match path_item_type {
        PathItemType::Get => "GET",
        PathItemType::Post => "POST",
        PathItemType::Put => "PUT",
        PathItemType::Delete => "DELETE",
        PathItemType::Options => "OPTIONS",
        PathItemType::Head => "HEAD",
        PathItemType::Patch => "PATCH",
        PathItemType::Trace => "TRACE",
        PathItemType::Connect => "CONNECT"
    }
}

pub fn get_deprecated_string(deprecated: &Deprecated) -> &str {
    match deprecated {
        Deprecated::True => "true",
        Deprecated::False=> "false"
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let env = Env::default()
        .filter_or("MY_LOG_LEVEL", "trace");
    env_logger::init_from_env(env);
    //get mq connection
    let conn = connect_to_rabbitmq().await.unwrap();

    //create channels, exchange, 
    let consumer_channel = conn.create_channel().await?;

    //bind to queue
    consumer_channel.queue_bind(TODO_QUEUE_NAME, TODO_EXCHANGE_STR, "", QueueBindOptions::default(), FieldTable::default()).await?;

    info!("Waiting to consume messages");
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
    info!("[*] Connected to neo4j");
    //begin consume loop

    while let Some(delivery) = consumer.next().await {
        let delivery = delivery.expect("error in consumer");
        delivery
            .ack(BasicAckOptions::default())
            .await
            .expect("ack");

        // //deserialize json value
        let data_str = String::from_utf8(delivery.data).unwrap();
        debug!("received {}", data_str);
        
        let message : MessageType = serde_json::from_str(data_str.as_str()).unwrap();

        let transaction = graph_conn.start_txn().await.unwrap();

        match message {
            MessageType::Todo(vec) => {
                for todo in vec as Vec<Todo> {
                    let mut query = format!("CREATE (n: Todo {{id: \"{}\", title: \"{}\", completed: \"{}\" }}) return n ", todo.id, todo.title, todo.completed);
                    trace!("{}", query);

                    transaction.run(Query::new(query)).await.expect("Could not execute query on neo4j graph");

                    query = format!("MATCH (a:Application) WHERE a.title = '{}' with a MATCH (t:Todo) WHERE t.id = '{}' with a,t MERGE (a)-[:hasTodo]-(t)", "todo_app_sqlite_axum", todo.id);
                    trace!("{}", query);
                    transaction.run(Query::new(query));
                }
            },
            //TODO: Implement putting the api spec within the graph, represent each endpoint as a node?
            MessageType::OpenApiSpec(spec) => {
                //decompose the api spec, create a node for the application itself, and nodes for each endpoint, which should have relationships to their operations.
                let query = format!(
                    "MERGE (o:Application {{ \
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
                trace!("{}", query);

                transaction.run(Query::new(query)).await.expect("Could not execute query on neo4j graph");
                //iterate through paths
                for (endpoint, path_item) in spec.paths.paths.iter() {
                    debug!("found endpoint \"{endpoint}\"");
                    //destructure operation
                    for (operation_type, operation) in path_item.operations.iter() {
                        let op_type = get_operation_str(&operation_type);
                        let op_id = operation.operation_id.clone().unwrap_or_default();
                        let mut is_deprecated = "";
                        let mut external_docs_url = String::from("");

                        if let Some(deprecated) = operation.deprecated.clone() {
                            match deprecated {
                                Deprecated::True => is_deprecated="true",
                                Deprecated::False =>is_deprecated="false"
                            }
                        }

                        if let Some(external_docs) = operation.external_docs.clone() {
                            external_docs_url = external_docs.url.clone();
                        }
                        debug!("found {op_type} operation with id \"{}\"", operation.operation_id.clone().unwrap_or_default());
                      
                        let mut operation_query = format!(
                        "MERGE (e:Endpoint {{ \
                            endpoint: '{}',\
                            operationId: '{}', \
                            description: '{}', \
                            operationType: '{}',\
                            isDeprecated: '{}', \
                            externalDocsUrl: '{}'
                        }}) RETURN e",
                        endpoint,
                        op_id.clone(),
                        operation.description.clone().unwrap_or_default(),
                        op_type,
                        is_deprecated,
                        external_docs_url
                        );
                        trace!("{}", operation_query);
                        
                        transaction.run(Query::new(operation_query)).await.expect("Could not execute query on neo4j graph");

                        //draw relationship back to app node
                        operation_query = format!("MATCH (a:Application) WHERE a.title = '{}' with a MATCH (e:Endpoint) WHERE e.operationId = '{}' WITH a,e MERGE (a)-[:hasEndpoint]->(e) ",spec.info.title ,op_id.clone());
                        trace!("{}", operation_query);
                        transaction.run(Query::new(operation_query)).await.expect("Could not execute query on neo4j graph");
                    }
                }
            }
            _ => todo!()
        }
        match transaction.commit().await {
            Ok(_) => {
                debug!("[*] Transaction Committed")
             },
             Err(e) => panic!("Error updating graph {}", e)
        }

    } //end consume loop

    Ok(())
}
