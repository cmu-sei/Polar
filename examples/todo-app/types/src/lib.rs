use utoipa::openapi::OpenApi;
use serde::{Serialize, Deserialize};

pub const TODO_QUEUE_NAME: &str = "todo";
pub const TODO_EXCHANGE_STR: &str = "todo-app";

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Todo {
    pub completed: bool,
    pub id: u16,
    pub title: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub enum MessageType {
    Todo(Vec<Todo>),
    OpenApiSpec(OpenApi)
}
