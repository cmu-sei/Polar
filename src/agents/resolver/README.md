# Resolver

## Overview

The `resolver` crate is an agent in the Polar observability framework responsible for **validating and enriching OCI artifacts** collected by various consumers (GitLab, ACR, Artifactory, Kubernetes, etc.). Its main purpose is to ensure that OCIRegistry and OCIArtifact nodes in the graph are verified and canonical, providing a trusted foundation for downstream provenance analysis.

In addition to automatically resolving OCI artifacts from a given list of registries, the `resolver` consumes "discovery" events emitted by other agents. These events describe newly discovered container images that may require verification. The resolver agent will:

- Pull container manifests 
- Extract and verify the image digest and media type.
- Emit "resolved" events to the broker, which are then consumed by the `provenance` agent to link container images to SBOMs, build pipelines, commits, and software components.

There's an explicit **trust boundary** being enforced by this design. It is also responsible for *proving* claims made by observer agents. Though observers scraping data from package and container registries may be able to access the data directly, their consumers are only interested in populating the knowledge graph with that information. When another observed system downstream of those repositories is found to be running some container image or using a package binary, the agents for that system have a similar responsibility. This agent is meant to bridge that knowledge gap by automatically "following their tracks", so to speak, and retrieving the necessary data (container image digests, package binary hashes, etc.) in order to inform the "provenance linker" to update the knowledge graph.


---

## Architecture & Workflow

```text
[Consumer] ── creates unverified ContainerImageRef nodes ──▶ [Resolver]
          └─ emits "discovery" events

[Resolver] ── validates and enriches refs ──▶ [ArtifactLinker]
          └─ emits "resolved" events with verified digests

[ArtifactLinker] ── consumes "resolved" events ──▶
   Upserts nodes and ensures edges between OCIRegistries, Artifacts, and ContainerImageReferences 

## Setup

The Artifact linker sits within a trusted zone as a graph facing agent. As such it should be configured with environment variables to specify the location of the knowledge graph database and the authentication credentials for it in addition to the values for connecting to the message broker.
