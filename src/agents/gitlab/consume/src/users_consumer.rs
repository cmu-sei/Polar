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
use helpers::helpers::{get_neo_config, run_query};
use common::{connect_to_rabbitmq, GITLAB_EXCHANGE_STR, USERS_QUEUE_NAME, USERS_ROUTING_KEY};
use lapin::{options::*, types::FieldTable, Result};
use log::{debug, error, info};

mod helpers;
#[tokio::main]
async fn main() -> Result<()> {

    //get mq connection
    let conn = connect_to_rabbitmq().await.unwrap();

    //create channels, exchange, 
    let consumer_channel = conn.create_channel().await?;

    //bind to queue
    consumer_channel.queue_bind(USERS_QUEUE_NAME, GITLAB_EXCHANGE_STR, USERS_ROUTING_KEY, QueueBindOptions::default(), FieldTable::default()).await?;

    info!("[*] waiting to consume");
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
    info!("[*] Connected to neo4j");
    //begin consume loop

    while let Some(result) = consumer.next().await {
        match result {
            Ok(delivery) => {
                delivery
                .ack(BasicAckOptions::default())
                .await
                .expect("ack");
    
            // //deserialize json value
            //TODO: What else can be done on these error cases? Log message data?
            let message : MessageType = match serde_json::from_slice(delivery.data.as_slice()) {
                Ok(msg) => msg,
                Err(e) => {
                    error!("Could not deserialize message! {}", e);
                    continue
                }
            };
            
            debug!("{:?}", message);
            
            let transaction = match graph_conn.start_txn().await {
                Ok(t) => t,
                Err(e) => {
                    error!("Could not open transaction with graph! {}", e);
                    continue
                }
            };
            
            match message {
                MessageType::Users(user_vec) => {
                    //build query from scratch
                    for user in user_vec as Vec<User> {
                        //create new nodes
                        let query = format!("MERGE (n:GitlabUser {{username: \"{}\", user_id: \"{}\" , created_at: \"{}\" , state: \"{}\"}}) return n", user.username, user.id, user.created_at.unwrap_or("".to_string()), user.state);            
                        if !run_query(&transaction, query).await {
                            continue;
                        }
                    }
                },
                _ => todo!()
                
            }
            match transaction.commit().await {
                Ok(_) => {
                    info!("[*] Transaction Committed")
                 },
                 Err(e) => error!("Error updating graph {}", e)
            }
    
            }Err(e) => {
                error!("Error getting message Delivery!{}", e );
                continue
            }
        };
    }
    Ok(())
}
