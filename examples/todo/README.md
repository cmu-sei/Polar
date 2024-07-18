# Todo Axum API

This project is a simple Todo API built using the Axum web framework in Rust. It provides endpoints to manage a list of todo items, including creating, listing, searching, updating, and deleting todos. The API also includes OpenAPI documentation for easy interaction and exploration.

## Getting Started

### Prerequisites

- Rust (latest stable version)
- Cargo (comes with Rust)

### Installation

Install dependencies and build the project:

```
cargo build
```

### Running the API

To run the API server, use the following command:

```
cargo run
```

The server will start on port `8000`. You can access it at `http://localhost:8000`. You can access the docs at `http://localhost:8000/docs`.

### Port Information

The default port for this API is set to `8000`. You can change the port by modifying the following line in the `main` function:

```
let address = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 8000));
```

## Endpoints

- **GET /docs**: Returns the HTML documentation generated from the OpenAPI schema.
- **GET /api/json**: Returns the OpenAPI JSON schema.

## Explanation

This program implements a simple RESTful API for managing todo items using the Axum framework. It includes the following functionalities:

- **List Todos**: Retrieve a list of all todo items.
- **Search Todos**: Search for todo items based on a description and completion status.
- **Create Todo**: Add a new todo item to the list.
- **Mark Todo as Done**: Update a todo item to mark it as completed.
- **Delete Todo**: Remove a todo item from the list.

The API also provides OpenAPI documentation to help users understand and interact with the available endpoints. This documentation can be accessed in HTML format at `/docs` or in JSON format at `/api/json`.


## Acknowledgments
The project was based on the [Utoipa](https://github.com/juhaku/utoipa) [TODO Example Project](https://github.com/juhaku/utoipa/tree/master/examples/todo-axum), but was heavily modified for this use cases.

