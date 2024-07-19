//TODO:
// Read todo themselves
// Know that it is there
// get all endpoints (/api/json)
// Make sure we have the datatypes? 

use serde::{Deserialize, Serialize};
use serde_json::to_string;
use oas3::from_str;
use lapin::types::FieldTable;
use lapin::options::{QueueBindOptions,QueueDeclareOptions,ExchangeDeclareOptions};
use common::{connect_to_rabbitmq,publish_message};


#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Todo {
    pub done: bool,
    pub id: u32,
    pub value: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub enum MessageType {
    Todo(Vec<Todo>)
}

pub const TODO_QUEUE_NAME: &str = "todo";
pub const TODO_EXCHANGE_STR: &str = "todo-app";

#[tokio::main]
async fn main() -> Result<(), oas3::Error> {
    // Fetch and print todos
    let _ = fetch_todos().await;

    // Parse OpenAPI specification from a local file or URL
    let spec = parse_openapi_spec("http://localhost:8000/api/json").await?;
    println!("OpenAPI Specification: {:?}", spec);

    Ok(())
}

async fn fetch_todos() -> Result<(), reqwest::Error> {
    
    let mq_conn = connect_to_rabbitmq().await.unwrap();

    // Create publish channel and exchange
    let mq_publish_channel = mq_conn.create_channel().await.unwrap();
    mq_publish_channel.exchange_declare(TODO_EXCHANGE_STR, lapin::ExchangeKind::Direct, 
    ExchangeDeclareOptions::default(),FieldTable::default()).await.unwrap();

    println!("[*] Gitlab Exchange Declared");

    //create publish channel
    // let mq_publish_channel = mq_conn.create_channel().await.unwrap();

    //create fresh queue, empty string prompts the server backend to create a random name
    let _ = mq_publish_channel.queue_declare(TODO_QUEUE_NAME,QueueDeclareOptions::default() , FieldTable::default()).await.unwrap();

    //bind queue to exchange so it sends messages where we need them
    mq_publish_channel.queue_bind(TODO_QUEUE_NAME, TODO_EXCHANGE_STR, "", QueueBindOptions::default(), FieldTable::default()).await.unwrap();

    println!("Retrieving todos");
    let todos = reqwest::get("http://localhost:8000/api/v1/todos")
        .await?
        .json::<Vec<Todo>>()
        .await?;

    // for todo in response {
    //     println!("ID: {}, Value: {}, Done: {}", todo.id, todo.value, todo.done);
    // }

    publish_message(to_string(&MessageType::Todo(todos.clone())).unwrap().as_bytes(), &mq_publish_channel, TODO_EXCHANGE_STR, "").await;
    

    Ok(())
}

async fn parse_openapi_spec(url: &str) -> Result<oas3::OpenApiV3Spec, oas3::Error> {
    let response_string: String = reqwest::get(url).await
        .unwrap()
        .text()
        .await.unwrap();
    
    match from_str(response_string) {
        Ok(spec) => Ok(spec),
        Err(err) => Err(err)
    }
}