use crate::{fallback::file_and_error_handler, todo::*};
use axum::{
    body::Body, extract::{Path, State}, http::Request, response::{IntoResponse, Response}, routing::{get, post}, Json, Router
};
use http::request::Parts;
use leptos::*;
use leptos_axum::{generate_route_list, LeptosRoutes};
use serde_json::json;
use todo_app_sqlite_axum::*;
use utoipa::OpenApi;
use utoipa_scalar::{Scalar, Servable as ScalarServable};

//Define a handler to test extractor with state
async fn custom_handler(
    Path(id): Path<String>,
    State(options): State<LeptosOptions>,
    req: Request<Body>,
) -> Response {
    let handler = leptos_axum::render_app_to_stream_with_context(
        options,
        move || {
            provide_context(id.clone());
        },
        TodoApp,
    );
    handler(req).await.into_response()
}

#[tokio::main]
async fn main() {
    use crate::todo::ssr::db;

    #[derive(OpenApi)]
    #[openapi(
        paths(add_todo,get_todos,delete_todo),
        tags(
            (name = "todo", description = "Todo items management API")
        )
    )]
    struct ApiDoc;

    simple_logger::init_with_level(log::Level::Error)
        .expect("couldn't initialize logging");

    let mut conn = db().await.expect("couldn't connect to DB");
    if let Err(e) = sqlx::migrate!().run(&mut conn).await {
        eprintln!("{e:?}");
    }

    // Generate the OpenAPI documentation
    let api_doc = ApiDoc::openapi();
    let openapi_json = json!(api_doc);

    // Setting this to None means we'll be using cargo-leptos and its env vars
    let conf = get_configuration(None).await.unwrap();
    let leptos_options = conf.leptos_options;
    let addr = leptos_options.site_addr;
    let routes = generate_route_list(TodoApp);

    // build our application with a route
    let app = Router::new()
        .merge(Scalar::with_url("/scalar", ApiDoc::openapi())) //interactive ui
        .route("/api/json", get(move || async move { Json(openapi_json.clone()) })) //expose openapi spec as json for agent
        .route("/api/todos", get(move || async move { //add "backend" fn to return json list
            
            let req_parts = use_context::<Parts>();

            if let Some(req_parts) = req_parts {
                println!("Uri = {:?}", req_parts.uri);
            }

            use futures::TryStreamExt;

            let mut conn = db().await.unwrap();

            let mut todos = Vec::new();
            let mut rows =
                sqlx::query_as::<_, Todo>("SELECT * FROM todos").fetch(&mut conn);
            while let Some(row) = rows.try_next().await.unwrap() {
                todos.push(row);
            }

            // Lines below show how to set status code and headers on the response
            // let resp = expect_context::<ResponseOptions>();
            // resp.set_status(StatusCode::IM_A_TEAPOT);
            // resp.insert_header(SET_COOKIE, HeaderValue::from_str("fizz=buzz").unwrap());
            Json(todos)
        }))
        .route("/special/:id", get(custom_handler))
        .leptos_routes(&leptos_options, routes, || view! { <TodoApp/> })
        .fallback(file_and_error_handler)
        .with_state(leptos_options);

    // run our app with hyper
    // `axum::Server` is a re-export of `hyper::Server`
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    logging::log!("listening on http://{}", &addr);
    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();
}
