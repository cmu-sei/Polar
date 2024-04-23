// Polar
// Copyright 2023 Carnegie Mellon University.
// NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.
// [DISTRIBUTION STATEMENT D] Distribution authorized to the Department of Defense and U.S. DoD contractors only (materials contain software documentation) (determination date: 2022-05-20). Other requests shall be referred to Defense Threat Reduction Agency.
// Notice to DoD Subcontractors:  This document may contain Covered Defense Information (CDI).  Handling of this information is subject to the controls identified in DFARS 252.204-7012 – SAFEGUARDING COVERED DEFENSE INFORMATION AND CYBER INCIDENT REPORTING
// Carnegie Mellon® is registered in the U.S. Patent and Trademark Office by Carnegie Mellon University.
// This Software includes and/or makes use of Third-Party Software subject to its own license, see license.txt file for more information. 
// DM23-0821
// 
use std::error::Error;
use gitlab_service::{get_all_elements, get_project_pipelines};
use gitlab_types::{Project, User, Runner, MessageType, ResourceLink, Pipeline};
use lapin::{options::{QueueDeclareOptions, QueueBindOptions}, types::FieldTable};
use reqwest::Client;
use serde_json::to_string;
use common::{create_lock, get_gitlab_token, get_gitlab_endpoint, connect_to_rabbitmq, publish_message, GITLAB_EXCHANGE_STR, PROJECTS_QUEUE_NAME, PROJECTS_ROUTING_KEY};
use std::fs::remove_file;

const LOCK_FILE_PATH: &str = "/tmp/projects_observer.lock";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error> > {
    let result = create_lock(LOCK_FILE_PATH);
    match result {
        Err(e) => panic!("{}", e),
        Ok(false) => Ok(()),
        Ok(true) => {
            println!("Running project task");

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

            let web_client = Client::builder().build().unwrap();

            println!("Retrieving projects");
            let projects: Vec<Project> = get_all_elements(&web_client, gitlab_token.clone(), format!("{}{}", service_endpoint.clone(), "/projects")).await.unwrap();
            publish_message(to_string(&MessageType::Projects(projects.clone())).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR, PROJECTS_ROUTING_KEY).await;
            
            for project in projects {
                //get users of each project
                let users: Vec<User> = get_all_elements(&web_client, gitlab_token.clone(), format!("{}{}{}{}", service_endpoint, "/projects/" , project.id, "/users")).await.unwrap();

                publish_message(to_string(&MessageType::ProjectUsers(ResourceLink {
                    resource_id: project.id,
                    resource_vec: users.clone()
                })).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR, PROJECTS_ROUTING_KEY).await;

                //get runners
                let runners: Vec<Runner> = get_all_elements(&web_client, gitlab_token.clone(), format!("{}{}{}{}", service_endpoint, "/projects/", project.id, "/runners")).await.unwrap();
                publish_message(to_string(&MessageType::ProjectRunners(ResourceLink { 
                    resource_id: project.id, 
                    resource_vec: runners.clone() }))
                    .unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR, PROJECTS_ROUTING_KEY).await;
                //get 20 projects pipelines
                
                let pipelines: Vec<Pipeline> = get_project_pipelines(&web_client, project.id, gitlab_token.clone(), service_endpoint.clone()).await.unwrap();

                publish_message(to_string(&MessageType::Pipelines(pipelines.clone())).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR, PROJECTS_ROUTING_KEY).await;

            }
            let _ = mq_conn.close(0, "closed").await?;

            //delete lock file
            remove_file(LOCK_FILE_PATH).expect("Error deleting lock file.");


            Ok(())
        }
    }
}
