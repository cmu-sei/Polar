# Example Observable Application

This directory contains a simple axum application that exposes a few endpoints documented with `utoipa` and the OpenApi specification.

The `agent` directory contains a basic agent that observes and processes information from the application, such as the current TODO's in the application and the endpoints it exposes.

## Running the example

Provided your environment is set up for rust development. You can build and run the application under `app` using cargo.

```
cargo run
```
Once the app is running, feel free to add some example data pieces to be observed

The web app will run on localhost listening on `3000` by default.


```bash
curl  -X POST \
  'http://localhost:3000/api/v1/todos' \
  --header 'Accept: */*' \
  --header 'Content-Type: application/json' \
  --data-raw '{
  "id": 1,
  "value": "Shop",
  "done": false
}'

curl  -X POST \
  'http://localhost:3000/api/v1/todos' \
  --header 'Accept: */*' \
  --header 'Content-Type: application/json' \
  --data-raw '{
  "id": 2,
  "value": "Make Dinner",
  "done": false
}'

curl  -X POST \
  'http://localhost:3000/api/v1/todos' \
  --header 'Accept: */*' \
  --header 'Content-Type: application/json' \
  --data-raw '{
  "id": 3,
  "value": "Wash the car",
  "done": false
}'

```
The example agent will require an instance of `cassini` and `neo4j` to be present, see docs for setup info.

* [Cassini](../../src/agents/broker/)
* [Building Agents](../../src/agents/README.md)


