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

use gitlab_service::{get_all_elements, get_project_pipelines};
use gitlab_types::{MessageType, ResourceLink};
use lapin::{options::{QueueDeclareOptions, QueueBindOptions}, types::FieldTable};
use serde_json::to_string;
use common::{create_lock, get_gitlab_token, get_gitlab_endpoint, connect_to_rabbitmq, publish_message, GITLAB_EXCHANGE_STR, PROJECTS_QUEUE_NAME, PROJECTS_ROUTING_KEY};
use std::{error::Error, fs::remove_file};
use log::{info, warn, error};
mod helpers;

const LOCK_FILE_PATH: &str = "/tmp/projects_observer.lock";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error> > {
    let result = create_lock(LOCK_FILE_PATH);
    match result {
        Err(e) => panic!("{}", e),
        Ok(false) => Ok(()),
        Ok(true) => {
            env_logger::init();

            info!("Running project task");

            //publish user id and list of user's projects to queue?
            let mq_conn = connect_to_rabbitmq().await?;
            
            let mq_publish_channel = mq_conn.create_channel().await?;

            //create fresh queue, empty string prompts the server backend to create a random name
            let _ = mq_publish_channel.queue_declare(PROJECTS_QUEUE_NAME,QueueDeclareOptions::default() , FieldTable::default()).await?;

            //bind queue to exchange so it sends messages where we need them
            mq_publish_channel.queue_bind(PROJECTS_QUEUE_NAME, GITLAB_EXCHANGE_STR, PROJECTS_ROUTING_KEY, QueueBindOptions::default(), FieldTable::default()).await?;

            //poll gitlab
            let gitlab_token = get_gitlab_token();
            let service_endpoint = get_gitlab_endpoint();

            let web_client = helpers::helpers::web_client();

            
            if let Some(projects) = get_all_elements(&web_client, gitlab_token.clone(), format!("{}{}", service_endpoint.clone(), "/projects")).await {
                publish_message(to_string(&MessageType::Projects(projects.clone())).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR, PROJECTS_ROUTING_KEY).await;
                for project in projects {
                    // get users of each project
                    if let Some(users) = get_all_elements(&web_client, gitlab_token.clone(), format!("{}{}{}{}", service_endpoint, "/projects/" , project.id, "/users")).await {
                        publish_message(to_string(&MessageType::ProjectUsers(ResourceLink {
                            resource_id: project.id,
                            resource_vec: users.clone()
                        })).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR, PROJECTS_ROUTING_KEY).await;
    
                    }
    
                    //get runners
                    if let Some(runners) = get_all_elements(&web_client, gitlab_token.clone(), format!("{}{}{}{}", service_endpoint, "/projects/", project.id, "/runners")).await {
                        publish_message(to_string(&MessageType::ProjectRunners(ResourceLink { 
                            resource_id: project.id, 
                            resource_vec: runners.clone() }))
                            .unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR, PROJECTS_ROUTING_KEY).await;    
                    }

                    // get 20 projects pipelines
                    //TODO: Get all runs of pipelines? Make configurable (number of pipeline runs to retrieve etc)
                    match get_project_pipelines(&web_client, project.id, gitlab_token.clone(), service_endpoint.clone()).await {
                        Ok(pipelines) => publish_message(to_string(&MessageType::Pipelines(pipelines.clone())).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR, PROJECTS_ROUTING_KEY).await,
                        Err(e) => error!("Could not get project {} pipelines, {}", project.id, e)
                    }
                    
                }
            }
            
            
            
            let _ = mq_conn.close(0, "closed").await?;

            //delete lock file
            remove_file(LOCK_FILE_PATH).unwrap_or_else(|_| {
                warn!("Error deleting lock file")
            });
            Ok(())
        }
    }
}
