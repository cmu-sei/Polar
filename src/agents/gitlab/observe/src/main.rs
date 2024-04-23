mod helpers;
use std::{error::Error};
use gitlab_service::{get_token, get_service_endpoint, get_all_elements, get_projects};
use gitlab_types::{User, MessageType, Project, UserGroup, Runner, Pipeline, Job, ResourceLink};
use lapin::{Connection, ConnectionProperties, options::{ExchangeDeclareOptions, QueueDeclareOptions, BasicPublishOptions, QueueBindOptions}, types::FieldTable, Channel, BasicProperties, publisher_confirm::Confirmation};
use reqwest::Client;
use serde_json::{to_string};
use crate::helpers::helpers::get_rabbit_endpoint;
const GITLAB_EXCHANGE_STR: &str = "gitlab_users";

async fn get_mq_conn(addr: String)  -> Connection {
    let conn = Connection::connect(
        &addr,
        ConnectionProperties::default(),
    )
    .await.expect("Could not connect to rabbit mq at given address");

    println!("[*] Connected to RabbitMq");

    return conn
}

async fn publish_message(payload: &[u8], channel: &Channel, exchange: &str){
    
    //No routing key, TODO, how to use this for our purposes?
    let confirmation = channel.basic_publish(exchange, "", 
    BasicPublishOptions::default(),
        payload,
        BasicProperties::default()).await.unwrap().await.unwrap();
    
    assert_eq!(confirmation, Confirmation::NotRequested);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error> > {
    let rabbitmq_host = get_rabbit_endpoint();
    //publish user id and list of user's projects to queue?
    let mq_conn = get_mq_conn(rabbitmq_host.to_string()).await;
    
    //create publish channel and exchange

    let mq_publish_channel = mq_conn.create_channel().await?;
    mq_publish_channel.exchange_declare("gitlab_users", lapin::ExchangeKind::Fanout, 
    ExchangeDeclareOptions::default(),FieldTable::default()).await?;

     //create fresh queue, empty string prompts the server backend to create a random name
    let queue = mq_publish_channel.queue_declare("users",QueueDeclareOptions::default() , FieldTable::default()).await?;

    //bind queue to exchange so it sends messages where we need them
    mq_publish_channel.queue_bind(queue.name().as_str(), GITLAB_EXCHANGE_STR, "", QueueBindOptions::default(), FieldTable::default()).await?;

    //poll gitlab for available users, groups, and projects
    let gitlab_token = get_token();
    let service_endpoint = get_service_endpoint();

    let web_client = Client::builder().build().unwrap();


    let mut endpoint: String = format!("{}{}", service_endpoint, "/projects");
    let projects: Vec<Project> = get_all_elements(&web_client, gitlab_token.clone(), endpoint).await.unwrap();
    println!("found {} projects", projects.len());
    publish_message(to_string(&MessageType::Projects(projects.clone())).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR).await;
    
    let users: Vec<User> = get_all_elements(&web_client, gitlab_token.clone(), format!("{}{}", service_endpoint, "/users")).await.unwrap();
    publish_message(to_string(&MessageType::Users(users.clone())).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR).await;
    
    endpoint = format!("{}{}", service_endpoint, "/groups");
    let groups: Vec<UserGroup> = get_all_elements(&web_client, gitlab_token.clone(), endpoint).await.unwrap();
    publish_message(to_string(&MessageType::Groups(groups.clone())).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR).await;
    
    endpoint = format!("{}{}", service_endpoint, "/runners/all");
    let runners: Vec<Runner> = get_all_elements(&web_client, gitlab_token.clone(), endpoint).await.unwrap();
    publish_message(to_string(&MessageType::Runners(runners.clone())).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR).await;

// //get project users and publish
    for project in projects {
        
        //get project users
        let users: Vec<User> = gitlab_service::get_all_elements(&web_client,
            gitlab_token.clone(),
            format!("{}{}{}",
                service_endpoint.clone(), 
                "/projects/".to_owned() + &project.id.to_string(),
                 "/users"))
            .await.unwrap();
    
        let message = MessageType::ProjectUsers(gitlab_types::ResourceLink { resource_id: project.id, resource_vec: users.clone() });
        let message_str = to_string(&message).unwrap();
        let payload = message_str.as_bytes();
        publish_message(payload, &mq_publish_channel, GITLAB_EXCHANGE_STR).await;
        
        //get project pipelines
        // endpoint = format!("{}{}{}{}", service_endpoint, "/projects/", project.id, "/pipelines");
        // let pipelines: Vec<Pipeline> = get_all_elements(&web_client, gitlab_token.clone(), endpoint).await.unwrap();
        // publish_message(to_string(&MessageType::Pipelines(pipelines.clone())).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR).await;

        // for pipeline in pipelines {
        //     endpoint = format!("{}{}{}{}{}{}", service_endpoint, "/projects/", project.id ,"/pipelines/",pipeline.id,"/jobs");
        //     let jobs: Vec<Job> = get_all_elements(&web_client, gitlab_token.clone(), endpoint).await.unwrap();
        //     publish_message(to_string(&MessageType::Jobs(jobs.clone())).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR).await;
        // }
    }
    
    // // get group data
    for group in groups {
        let members: Vec<User> = get_all_elements(&web_client,
            gitlab_token.clone(),
            format!("{}{}{}",
                     service_endpoint.clone(), 
                     "/groups/".to_string() + &group.id.to_string(),
                     "/members")).await.unwrap();
        publish_message(to_string(&MessageType::GroupMembers(
            gitlab_types::ResourceLink {
            resource_id: group.id, 
            resource_vec: members.clone()
        }))
        .unwrap().as_bytes(),
        &mq_publish_channel,
        GITLAB_EXCHANGE_STR).await;   
    }

    // 

    
    //get runner jobs
        
    // for runner in runners {
    //     endpoint = format!("{}{}{}{}", service_endpoint, "/runners/",runner.id,"/jobs");  
    //     let jobs: Vec<Job> = get_all_elements(&web_client, gitlab_token.clone(), endpoint).await.unwrap();
    //     publish_message(to_string(&MessageType::Jobs(jobs.clone())).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR).await;
    //     for j in jobs {
    //         publish_message(to_string(&MessageType::RunnerJob((runner.id, j))).unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR).await;
    //     }
    // }
    
    // //publish relationships
    // publish_message(to_string(&MessageType::PipelineJobs(ResourceLink {
    //     resource_id: 8096, resource_vec: jobs.clone()}))
    //     .unwrap().as_bytes(), &mq_publish_channel, GITLAB_EXCHANGE_STR).await;

    println!("[*] Messages published!");
    Ok(())
}
