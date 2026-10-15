<!--
  ACE Tool

  Copyright 2026 Carnegie Mellon University.

  NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
  FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
  EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
  PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
  MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
  TRADEMARK, OR COPYRIGHT INFRINGEMENT.

  Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
  for full terms.

  [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
  distribution.  Please see Copyright notice for non-US Government use and distribution.

  This Software includes and/or makes use of Third-Party Software each subject to its own license.

  DM26-0670
-->

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
