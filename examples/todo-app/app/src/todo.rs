use crate::error_template::ErrorTemplate;
use error::Error;
use leptos::*;
use leptos_meta::*;
use leptos_router::*;
use serde::{Deserialize, Serialize};
use server_fn::codec::SerdeLite;
use utoipa::{IntoParams, OpenApi, ToSchema};

#[derive(OpenApi)]
#[openapi(
    paths(get_todos, add_todo, delete_todo,),
    components(schemas(Todo))
)]
pub(super) struct TodoApi;


#[derive(Clone, Debug, ToSchema, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(sqlx::FromRow))]
pub struct Todo {  
   pub id: u16,
    #[schema(example = "Buy groceries")]
    pub title: String,
    pub completed: bool,
}

#[cfg(feature = "ssr")]
pub mod ssr {
    
    // use http::{header::SET_COOKIE, HeaderMap, HeaderValue, StatusCode};
    use leptos::ServerFnError;
    use sqlx::{Connection, SqliteConnection};

    pub async fn db() -> Result<SqliteConnection, ServerFnError> {
        //read dbfile from env var to read other todo dbs. 

        let db_file = std::env::var("DB_FILE").unwrap();
        Ok(SqliteConnection::connect(format!("sqlite:{}", db_file).as_str()).await?)
    }
}

#[utoipa::path(
    get,
    path = "",
    responses(
        (status = 200, description = "List all todos successfully")
    )
)]
#[server]
pub async fn get_todos() -> Result<Vec<Todo>, ServerFnError> {
    use self::ssr::*;
    use http::request::Parts;

    // this is just an example of how to access server context injected in the handlers
    let req_parts = use_context::<Parts>();

    if let Some(req_parts) = req_parts {
        println!("Uri = {:?}", req_parts.uri);
    }

    use futures::TryStreamExt;

    let mut conn = db().await?;

    let mut todos = Vec::new();
    let mut rows =
        sqlx::query_as::<_, Todo>("SELECT * FROM todos").fetch(&mut conn);
    while let Some(row) = rows.try_next().await? {
        todos.push(row);
    }

    // Lines below show how to set status code and headers on the response
    // let resp = expect_context::<ResponseOptions>();
    // resp.set_status(StatusCode::IM_A_TEAPOT);
    // resp.insert_header(SET_COOKIE, HeaderValue::from_str("fizz=buzz").unwrap());

    Ok(todos)
}

#[utoipa::path(
    post,
    path = "",
    request_body = title,
    responses(
        (status = 200, description = "Todo item created successfully"),
        (status = 500, description = "Something went wrong")
    )
)]
#[server]
pub async fn add_todo(title: String) -> Result<(), ServerFnError> {
    use self::ssr::*;
    let mut conn = db().await?;

    // fake API delay
    std::thread::sleep(std::time::Duration::from_millis(1250));

    match sqlx::query("INSERT INTO todos (title, completed) VALUES ($1, false)")
        .bind(title)
        .execute(&mut conn)
        .await
    {
        Ok(_row) => Ok(()),
        Err(e) => Err(ServerFnError::ServerError(e.to_string())),
    }
}

#[utoipa::path(
    delete,
    path = "/{id}",
    responses(
        (status = 200, description = "Todo marked done successfully"),
        (status = 500, description = "Something went wrong")
      
    ),
    params(
        ("id" = u16, Path, description = "Todo database id")
    )
)]
#[server(output = SerdeLite)]
pub async fn delete_todo(id: u16) -> Result<(), ServerFnError> {
    use self::ssr::*;
    let mut conn = db().await?;

    Ok(sqlx::query("DELETE FROM todos WHERE id = $1")
        .bind(id)
        .execute(&mut conn)
        .await
        .map(|_| ())?)
}

//TODO: Add get_todo fn and document with Scalar
// pub async fn get_todo_by_id(id: u16) -> Result<String, Error> {
                
//     let mut conn = db().await?;
        
//     let mut row = sqlx::query("SELECT FROM todos WHERE id = $1")
//     .bind(id)
//     .fetch_one(&mut conn).await.unwrap();

//     let todo = Todo {
//         id: row.get(0),
//         title: row.get(1),
//         completed: row.get(2)
//     };
    
//     let json = serde_json::to_string(&todo).unwrap();

//     Ok(json)
// }

#[component]
pub fn TodoApp() -> impl IntoView {
    //let id = use_context::<String>();
    provide_meta_context();
    view! {
        <Link rel="shortcut icon" type_="image/ico" href="/favicon.ico"/>
        <Stylesheet id="leptos" href="/pkg/todo_app_sqlite_axum.css"/>
        <Router>
            <header>
                <h1>"My Tasks"</h1>
            </header>
            <main>
                <Routes>
                    <Route path="" view=Todos/>
                </Routes>
            </main>
        </Router>
    }
}

#[component]
pub fn Todos() -> impl IntoView {
    let add_todo = create_server_multi_action::<AddTodo>();
    let delete_todo = create_server_action::<DeleteTodo>();
    let submissions = add_todo.submissions();

    // list of todos is loaded from the server in reaction to changes
    let todos = create_resource(
        move || (add_todo.version().get(), delete_todo.version().get()),
        move |_| get_todos(),
    );

    view! {
        <div>
            <MultiActionForm action=add_todo>
                <label>
                    "Add a Todo"
                    <input type="text" name="title"/>
                </label>
                <input type="submit" value="Add"/>
            </MultiActionForm>
            <Transition fallback=move || view! {<p>"Loading..."</p> }>
                <ErrorBoundary fallback=|errors| view!{<ErrorTemplate errors=errors/>}>
                    {move || {
                        let existing_todos = {
                            move || {
                                todos.get()
                                    .map(move |todos| match todos {
                                        Err(e) => {
                                            view! { <pre class="error">"Server Error: " {e.to_string()}</pre>}.into_view()
                                        }
                                        Ok(todos) => {
                                            if todos.is_empty() {
                                                view! { <p>"No tasks were found."</p> }.into_view()
                                            } else {
                                                todos
                                                    .into_iter()
                                                    .map(move |todo| {
                                                        view! {

                                                            <li>
                                                                {todo.id}: {todo.title}
                                                                <ActionForm action=delete_todo>
                                                                    <input type="hidden" name="id" value={todo.id}/>
                                                                    <input type="submit" value="X"/>
                                                                </ActionForm>
                                                            </li>
                                                        }
                                                    })
                                                    .collect_view()
                                            }
                                        }
                                    })
                                    .unwrap_or_default()
                            }
                        };

                        let pending_todos = move || {
                            submissions
                            .get()
                            .into_iter()
                            .filter(|submission| submission.pending().get())
                            .map(|submission| {
                                view! {

                                    <li class="pending">{move || submission.input.get().map(|data| data.title) }</li>
                                }
                            })
                            .collect_view()
                        };

                        view! {

                            <ul>
                                {existing_todos}
                                {pending_todos}
                            </ul>
                        }
                    }
                }
                </ErrorBoundary>
            </Transition>
        </div>
    }
}
