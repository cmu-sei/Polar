use std::net::{Ipv4Addr, SocketAddr};
use axum::routing::get_service;
use axum::{response::Html, routing::get, Router, Json};
use hyper::Server;
use serde_json::json;
use std::io::Error;
use tower_http::services::ServeDir;
use utoipa::OpenApi;
use utoipa_scalar::Scalar;

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Define the OpenAPI documentation for the API
    #[derive(OpenApi)]
    #[openapi(
        paths(todo::list_todos, todo::search_todos, todo::create_todo, todo::mark_done, todo::delete_todo),
        tags(
            (name = "todo", description = "Todo items management API")
        ),
        servers(
            (url = "http://localhost:8000/api/v1/todos")
        )
    )]
    struct ApiDoc;

    // Generate the OpenAPI documentation
    let api_doc = ApiDoc::openapi();
    let scalar = Scalar::new(json!(api_doc));
    let openapi_json = json!(api_doc);

    // Define the Axum router
    let app = Router::new()
        .route("/docs", get(move || async move { Html(scalar.to_html()) }))
        .route("/api/json", get(move || async move { Json(openapi_json.clone()) }))
        .nest("/api/v1/todos", todo::router())
        .fallback_service(get_service(ServeDir::new("./static")));

    // Bind the server to the specified address and port
    let address = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 8000));
    let _ = Server::bind(&address).serve(app.into_make_service()).await;

    Ok(())
}

mod todo {
    use axum::{
        extract::{Path, Query, State},
        response::IntoResponse,
        routing, Json, Router,
    };
    use hyper::StatusCode;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use utoipa::{IntoParams, ToSchema};

    type Store = Mutex<Vec<Todo>>;

    // Define the Todo struct
    #[derive(Serialize, Deserialize, ToSchema, Clone)]
    struct Todo {
        /// The task description
        #[schema(example = "Buy groceries")]
        value: String,
        /// Task completion status
        #[schema(example = false)]
        done: bool,
        /// Todo ID
        #[schema(example = 1)]
        id: i32,
    }

    // Define the TodoError enum
    #[derive(Serialize, Deserialize, ToSchema)]
    enum TodoError {
        /// Conflict error
        #[schema(example = "Todo already exists")]
        Conflict(String),
        /// Not found error
        #[schema(example = "id = 1")]
        NotFound(String),
    }

    // Define the router for the Todo API
    pub(super) fn router() -> Router {
        let store = Arc::new(Store::default());
        Router::new()
            .route("/", routing::get(list_todos).post(create_todo))
            .route("/search", routing::get(search_todos))
            .route("/:id", routing::put(mark_done).delete(delete_todo))
            .with_state(store)
    }

    #[utoipa::path(
        get,
        path = "",
        responses(
            (status = 200, description = "List all todos successfully", body = [Todo], example = json!([{"id": 1, "value": "Buy groceries", "done": false}]))
        )
    )]
    /// List all todos
    ///
    /// This endpoint retrieves all todo items from the store.
    async fn list_todos(State(store): State<Arc<Store>>) -> Json<Vec<Todo>> {
        let todos = store.lock().await.clone();
        Json(todos)
    }

    #[derive(Deserialize, IntoParams, ToSchema)]
    struct TodoSearchQuery {
        /// Search query for task description
        #[schema(example = "Buy groceries")]
        value: String,
        /// Filter tasks by completion status
        #[schema(example = false)]
        done: bool,
    }

    #[utoipa::path(
        get,
        path = "/search",
        params(
            ("value" = String, Query, description = "Search query for task description"),
            ("done" = bool, Query, description = "Filter tasks by completion status")
        ),
        responses(
            (status = 200, description = "List matching todos by query", body = [Todo], example = json!([{"id": 1, "value": "Buy groceries", "done": false}]))
        )
    )]
    /// Search todos
    ///
    /// This endpoint searches for todo items that match the given query parameters.
    async fn search_todos(
        State(store): State<Arc<Store>>,
        query: Query<TodoSearchQuery>,
    ) -> Json<Vec<Todo>> {
        Json(
            store
                .lock()
                .await
                .iter()
                .filter(|todo| {
                    todo.value.to_lowercase() == query.value.to_lowercase()
                        && todo.done == query.done
                })
                .cloned()
                .collect(),
        )
    }

    #[utoipa::path(
        post,
        path = "",
        request_body(
            content = Todo,
            description = "The todo item to create",
            content_type = "application/json",
            example = json!({"id": 1, "value": "Buy groceries", "done": false})
        ),
        responses(
            (status = 201, description = "Todo item created successfully", body = Todo, example = json!({"id": 1, "value": "Buy groceries", "done": false})),
            (status = 409, description = "Todo already exists", body = TodoError, example = json!({"error": "Todo already exists"}))
        )
    )]
    /// Create a new todo
    ///
    /// This endpoint creates a new todo item and adds it to the store.
    async fn create_todo(
        State(store): State<Arc<Store>>,
        Json(todo): Json<Todo>,
    ) -> impl IntoResponse {
        let mut todos = store.lock().await;

        todos
            .iter_mut()
            .find(|existing_todo| existing_todo.id == todo.id)
            .map(|found| {
                (
                    StatusCode::CONFLICT,
                    Json(TodoError::Conflict(format!(
                        "todo already exists: {}",
                        found.id
                    ))),
                )
                    .into_response()
            })
            .unwrap_or_else(|| {
                todos.push(todo.clone());
                (StatusCode::CREATED, Json(todo)).into_response()
            })
    }

    #[utoipa::path(
        put,
        path = "/{id}",
        params(
            ("id" = i32, Path, description = "Todo database id")
        ),
        responses(
            (status = 200, description = "Todo marked done successfully", example = json!({"status": "success"})),
            (status = 404, description = "Todo not found", example = json!({"error": "Todo not found"}))
        )
    )]
    /// Mark todo as done
    ///
    /// This endpoint marks the specified todo item as done.
    async fn mark_done(Path(id): Path<i32>, State(store): State<Arc<Store>>) -> StatusCode {
        let mut todos = store.lock().await;

        todos
            .iter_mut()
            .find(|todo| todo.id == id)
            .map(|todo| {
                todo.done = true;
                StatusCode::OK
            })
            .unwrap_or(StatusCode::NOT_FOUND)
    }

    #[utoipa::path(
        delete,
        path = "/{id}",
        params(
            ("id" = i32, Path, description = "Todo database id")
        ),
        responses(
            (status = 200, description = "Todo deleted successfully"),
            (status = 404, description = "Todo not found", body = TodoError, example = json!(TodoError::NotFound(String::from("id = 1"))))
        )
    )]
    /// Delete todo
    ///
    /// This endpoint deletes the specified todo item from the store.
    async fn delete_todo(
        Path(id): Path<i32>,
        State(store): State<Arc<Store>>,
    ) -> impl IntoResponse {
        let mut todos = store.lock().await;

        let len = todos.len();

        todos.retain(|todo| todo.id != id);

        if todos.len() != len {
            StatusCode::OK.into_response()
        } else {
            (
                StatusCode::NOT_FOUND,
                Json(TodoError::NotFound(format!("id = {id}"))),
            )
                .into_response()
        }
    }
}
