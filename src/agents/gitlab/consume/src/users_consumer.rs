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

use futures_lite::StreamExt;
use gitlab_types::{MessageType, User};
use helpers::helpers::get_neo_config;
use common::{connect_to_rabbitmq, GITLAB_EXCHANGE_STR, USERS_QUEUE_NAME, USERS_ROUTING_KEY};
use lapin::{options::*, types::FieldTable, Result};
use neo4rs::Query;
mod helpers;
#[tokio::main]
async fn main() -> Result<()> {

    //get mq connection
    let conn = connect_to_rabbitmq().await.unwrap();

    //create channels, exchange, 
    let consumer_channel = conn.create_channel().await?;

    //bind to queue
    consumer_channel.queue_bind(USERS_QUEUE_NAME, GITLAB_EXCHANGE_STR, USERS_ROUTING_KEY, QueueBindOptions::default(), FieldTable::default()).await?;

    println!("[*] waiting to consume");
    let mut consumer = consumer_channel
    .basic_consume(
        USERS_QUEUE_NAME,
        "users_consumer",
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
        
        //begin transaction
        let transaction = graph_conn.start_txn().await.unwrap();

        match message {
            MessageType::Users(user_vec) => {
                //build query from scratch
                for user in user_vec as Vec<User> {
                    //create new nodes
                    let query = format!("MERGE (n:GitlabUser {{username: \"{}\", user_id: \"{}\" , created_at: \"{}\" , state: \"{}\"}}) return n", user.username, user.id, user.created_at.unwrap_or("".to_string()), user.state);            
                    //execute
                    transaction.run(Query::new(query)).await.expect("Could not execute query on neo4j graph");
                }
            },
            _ => todo!()
            
        }
        match transaction.commit().await {
            Ok(_) => {
                println!("[*] Transaction Committed")
             },
             Err(e) => panic!("Error updating graph {}", e)
        }

    } //end consume looopa
    Ok(())
}
