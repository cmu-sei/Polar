//TODO:
// Read todo themselves
// Know that it is there
// get all endpoints (/api/json)
// Make sure we have the datatypes? 



use serde::Deserialize;
use reqwest::Error;
use oas3::from_str;

#[derive(Debug, Deserialize)]
pub struct Todo {
    pub done: bool,
    pub id: u32,
    pub value: String,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Fetch and print todos
    fetch_todos().await?;

    // Parse OpenAPI specification from a local file or URL
    let spec = parse_openapi_spec("http://localhost:8000/api/json").await?;
    println!("OpenAPI Specification: {:?}", spec);

    Ok(())
}

async fn fetch_todos() -> Result<(), Error> {
    let response = reqwest::get("http://localhost:8000/api/v1/todos")
        .await?
        .json::<Vec<Todo>>()
        .await?;

    for todo in response {
        println!("ID: {}, Value: {}, Done: {}", todo.id, todo.value, todo.done);
    }

    Ok(())
}

async fn parse_openapi_spec(url: &str) -> Result<oas3::OpenApiV3Spec, Error> {
    let response: &str = reqwest::get(url).await?.text().await.unwrap().as_ref();
    println!("{}",response);
    match oas3::from_str(response) {
        Ok(spec) => Ok(spec),
        Err(err) => Err(err)
      };
       panic!("Unable to find header")
}