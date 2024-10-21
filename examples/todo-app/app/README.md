# Leptos Todo App Sqlite with Axum

This example creates a basic todo app with an Axum backend that uses Leptos' server functions to call sqlx from the client and seamlessly run it on the server.
Api endpoints are detailed with the swagger ui utilizing Utopia, a Rust crate for programmatically documenting APIs

## Getting Started
This project assumes you have already installed `cargo leptos`, are using rust nightly, and have already added the webassembly compilation target `wasm32-unknown-unknown` via rustup. See the Leptos documentation for more details.

See the Examples README from the main leptos repository

Be sure to provide the absolute path to a sqllite db file to the DB_FILE environment variable.

```fish
set -xg DB_FILE $(pwd)/Todos.db
```

```bash
export DB_FILE=$(pwd)/Todos.db
```

## Quick Start

Run `cargo leptos watch` to run this example.
