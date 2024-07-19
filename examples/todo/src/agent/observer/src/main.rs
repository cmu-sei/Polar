//TODO:
// Read todo themselves
// Know that it is there
// get all endpoints (/api/json)
// Make sure we have the datatypes? 



use serde::Deserialize;
use oas3::from_str;

#[derive(Debug, Deserialize)]
pub struct Todo {
    pub done: bool,
    pub id: u32,
    pub value: String,
}

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
    let response = reqwest::get("http://localhost:8000/api/v1/todos")
        .await?
        .json::<Vec<Todo>>()
        .await?;

    for todo in response {
        println!("ID: {}, Value: {}, Done: {}", todo.id, todo.value, todo.done);
    }

    Ok(())
}

async fn parse_openapi_spec(url: &str) -> Result<oas3::OpenApiV3Spec, oas3::Error> {
    let response_string: String = reqwest::get(url).await
        .unwrap()
        .text()
        .await.unwrap();
    
    println!("{}",response_string);

    match from_str(response_string) {
        Ok(spec) => Ok(spec),
        Err(err) => Err(err)
    }
}