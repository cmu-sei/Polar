/*
Polar (OSS)

Copyright 2024 Carnegie Mellon University.

NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for full terms.

[DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited distribution.  Please see Copyright notice for non-US Government use and distribution.

This Software includes and/or makes use of Third-Party Software each subject to its own license.

DM24-0470
*/

use std::{error::Error, fs::remove_file};
use gitlab_service::get_all_elements;
use gitlab_types::{UserGroup, MessageType, ResourceLink, User, Runner, Project};
use lapin::{options::{QueueDeclareOptions, QueueBindOptions}, types::FieldTable};
use reqwest::Client;
use serde_json::to_string;
use common::{create_lock, get_gitlab_token, get_gitlab_endpoint, connect_to_rabbitmq, publish_message, GITLAB_EXCHANGE_STR, GROUPS_ROUTING_KEY, GROUPS_QUEUE_NAME};

const LOCK_FILE_PATH: &str = "/tmp/groups_observer.lock";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error> > {
    let result = create_lock(LOCK_FILE_PATH);
    match result {
        Err(e) => panic!("{}", e),
        Ok(false) => Ok(()),
        Ok(true) => {
            println!("running groups task");

            //publish user id and list of user's projects to queue?
            let mq_conn = connect_to_rabbitmq().await?;
            
            //create publish channel

            let mq_publish_channel = mq_conn.create_channel().await?;

            //create fresh queue, empty string prompts the server backend to create a random name
            let _ = mq_publish_channel.queue_declare(GROUPS_QUEUE_NAME,QueueDeclareOptions::default() , FieldTable::default()).await?;

            //bind queue to exchange so it sends messages where we need them
            mq_publish_channel.queue_bind(GROUPS_QUEUE_NAME, GITLAB_EXCHANGE_STR, GROUPS_ROUTING_KEY, QueueBindOptions::default(), FieldTable::default()).await?;

            //poll gitlab for available users
            let gitlab_token = get_gitlab_token();
            let service_endpoint = get_gitlab_endpoint();

            let web_client = Client::builder().build().unwrap();
            println!("Retrieving groups");
            let groups: Vec<UserGroup> = get_all_elements(&web_client, gitlab_token.clone(), format!("{}{}", service_endpoint, "/groups")).await.unwrap();
            publish_message(to_string(&MessageType::Groups(groups.clone())).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR, GROUPS_ROUTING_KEY).await;

            for group in groups {
                //get users of each project
                let users: Vec<User> = get_all_elements(&web_client, gitlab_token.clone(), format!("{}{}{}{}", service_endpoint, "/groups/" , group.id, "/members")).await.unwrap();

                publish_message(to_string(&MessageType::GroupMembers(ResourceLink {
                    resource_id: group.id,
                    resource_vec: users.clone()
                })).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR, GROUPS_ROUTING_KEY).await;
                println!("[*] Group Users published!");
                
                //get group runners
                let runners: Vec<Runner> = get_all_elements(&web_client, gitlab_token.clone(), format!("{}{}{}{}", service_endpoint, "/groups/", group.id, "/runners")).await.unwrap();
                publish_message(to_string(&MessageType::GroupRunners(ResourceLink {
                    resource_id: group.id,
                    resource_vec: runners.clone()
                })).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR, GROUPS_ROUTING_KEY).await;

                // get all group's projects
                let projects: Vec<Project> = get_all_elements(&web_client, gitlab_token.clone(), format!("{}{}{}{}", service_endpoint, "/groups/", group.id, "/projects")).await.unwrap();
                publish_message(to_string(&MessageType::GroupProjects(ResourceLink {
                    resource_id: group.id,
                    resource_vec: projects.clone()
                })).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR, GROUPS_ROUTING_KEY).await;
                
            }
            
            remove_file(LOCK_FILE_PATH).expect("Error deleting lock file");

            let _ = mq_conn.close(0, "closed").await?;

            Ok(())
        }
    }
}
