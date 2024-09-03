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

use std::{error::Error, fs::remove_file};
use gitlab_service::get_all_elements;
use gitlab_types::{User, MessageType};
use lapin::{options::{QueueDeclareOptions, QueueBindOptions}, types::FieldTable};
use reqwest::Client;
use serde_json::to_string;
use common::{get_gitlab_token, get_gitlab_endpoint, connect_to_rabbitmq, publish_message, GITLAB_EXCHANGE_STR, USERS_QUEUE_NAME, USERS_ROUTING_KEY, create_lock};
use log::{debug, error, info, warn};
const LOCK_FILE_PATH: &str = "/tmp/users_observer.lock";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error> > {
    let result = create_lock(LOCK_FILE_PATH);
    match result {
        Err(e) => panic!("{}", e),
        Ok(false) => Ok(()),
        Ok(true) => {
            info!("Starting users task.");
            env_logger::init();
            //publish user id and list of user's projects to queue?
            let mq_conn = connect_to_rabbitmq().await?;
            
            //create publish channel
            let mq_publish_channel = mq_conn.create_channel().await?;

            //create fresh queue, empty string prompts the server backend to create a random name
            let _ = mq_publish_channel.queue_declare(USERS_QUEUE_NAME,QueueDeclareOptions::default() , FieldTable::default()).await?;

            //bind queue to exchange so it sends messages where we need them
            mq_publish_channel.queue_bind(USERS_QUEUE_NAME, GITLAB_EXCHANGE_STR, USERS_ROUTING_KEY, QueueBindOptions::default(), FieldTable::default()).await?;

            // Poll gitlab for available users
            let gitlab_token = get_gitlab_token();
            let service_endpoint = get_gitlab_endpoint();

            let web_client = Client::builder().build().unwrap();
            
            let users: Vec<User> = get_all_elements(&web_client, gitlab_token.clone(), format!("{}{}", service_endpoint, "/users")).await.unwrap();
            publish_message(to_string(&MessageType::Users(users.clone())).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR, USERS_ROUTING_KEY).await;
            
            let _ = mq_conn.close(0, "closed").await?;

            remove_file(LOCK_FILE_PATH).unwrap_or_else(|_| {
                warn!("Error deleting lock file")
            });
            
            Ok(())
        },
    }
}
