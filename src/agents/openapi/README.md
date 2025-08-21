# OpenAPI Spec Agents

This repository contains a set of agents designed to **discover, fetch, and consume OpenAPI specifications** exposed by applications. The agents are intended for observability and integration use cases, making it easy to centralize API knowledge and act on it programmatically.

## Overview

Many modern services expose an OpenAPI (Swagger) document at a known endpoint (e.g., `/openapi.json` or `/swagger.json`). These agents automate the process of:

1. **Discovery** – locating the OpenAPI spec from a target application.
2. **Retrieval** – fetching and validating the spec over HTTP.
3. **Consumption** – parsing the spec and exposing it in machine-friendly formats for downstream tools (e.g., graph databases, dashboards, risk evaluators).
4. **Observation** – tracking changes over time, surfacing diffs, and flagging potential contract or compliance issues.

This enables teams to continuously monitor their API surfaces and integrate that knowledge into the knowledge graph.

## Features

* Automatic retrieval of OpenAPI documents from a given base URL.
* Spec validation against the OpenAPI schema.
* Normalization into a consistent internal representation.
* Error handling and retries for services with intermittent availability.

## Usage

### Configuration

Other than the general environment variables related to TLS certificates and Neo4j credentials,
the web observer requires the `OPENAPI_ENDPOINT` variable be set to a valid endpoint where the openapi spec in can be retrieved via GET request in JSON format. For example

```export OPENAPI_ENDPOINT=http://localhost:3000/api-docs/openapi.json```

## Integration
