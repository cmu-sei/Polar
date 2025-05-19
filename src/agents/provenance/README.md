# Provenance Agent

The **Provenance Agent** is a stateless background service responsible for tracing the origin and lifecycle of software artifacts within the graph database. It analyzes existing data in Neo4j and constructs relationships between previously disconnected entities such as Pods, Container Images, Pipelines, Commits, and Charts.

## Purpose

This agent enhances the graph by identifying and creating provenance links between resources that are otherwise ingested separately — such as from Kubernetes and GitLab — and connecting them using naming conventions, metadata, and tags.

## Responsibilities

- Periodically scan the Neo4j graph for unlinked, but correlatable nodes
- Use container image references, commit hashes, and artifact names to create edges such as:
  - `(:PodContainer)-[:USES_TAG]->(:ContainerImageTag)`
  - `(:ContainerImageTag)-[:BUILT_BY]->(:GitlabJob)`
  - `(:GitlabJob)-[:PRODUCED]->(:BinaryPackage)`
- Annotate the graph with timestamped checks or markers to avoid redundant processing

## Design Philosophy

- **Passive analysis**: does not listen to real-time events; instead runs on a cadence set by operators
- **Query-driven**: leverages Cypher queries to discover relationships from current graph state
- **Decoupled**: does not interact with other services.
- **Idempotent**: safe to run repeatedly; avoids creating duplicate relationships

## Example Use Case

If a Pod in the graph has an image reference such as:

```
registry.exmaple.gitlab.com/some-namespace/some-image/tag:version

```

and a `ContainerImageTag` node exists with that location, the agent will match them and create:

```

(\:PodContainer)-\[:USES\_TAG]->(\:ContainerImageTag)

```

This forms the foundation for further traceability from runtime workloads back to their source.

---

> The Provenance Agent helps answer critical security and observability questions like:
> 
> _"What Git commit or pipeline produced this container?"_  
> _"Is this image running in production traceable to source?"_

### Setup

Setup for this agent is similar that of a consumer, and thus needs to be provided the same environment variables for credentials and possible CA certificates. Once those are set, you can run your typical `cargo` commands to build and run the agent.

