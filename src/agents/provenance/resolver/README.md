# Resolver

## Overview

The `resolver` crate is an agent in the Polar observability framework responsible for **validating and enriching container image references** collected by various consumers (GitLab, ACR, Artifactory, Kubernetes, etc.). Its main purpose is to ensure that `ContainerImageReference` nodes in the graph are verified and canonical, providing a trusted foundation for downstream provenance analysis.

Rather than directly scraping data, the `resolver` consumes "discovery" events emitted by observer or consumer agents. These events describe newly discovered container images that may require verification. The resolver agent will:

- Pull container manifests using tools like `skopeo` or similar.
- Extract and verify the image digest and media type.
- Download SBOMs and validate the claims submitted by pipeline artifact consumers against runtime registries and infrastructure, if possible.
- Emit "resolved" events to the broker, which are then consumed by the `provenance` agent to link container images to SBOMs, build pipelines, commits, and software components.

There's an explicit **trust boundary** being enforced by this design. It is also responsible for *proving* claims made by observer agents. Though observers scraping data from package and container registries may be able to access the data directly, their consumers are only interested in populating the knowledge graph with that information. When another observed system downstream of those repositories is found to be running some container image or using a package binary, the agents for that system have a similar responsibility. This agent is meant to bridge that knowledge gap by automatically "following their tracks", so to speak, and retrieving the necessary data (container image digests, package binary hashes, etc.) in order to inform the "provenance linker" to update the knowledge graph.


---

## Architecture & Workflow

```text
[Consumer] ── creates unverified ContainerImageRef nodes ──▶ [Resolver]
          └─ emits "discovery" events

[Resolver] ── validates and enriches refs using Skopeo ──▶ [Provenance]
          └─ emits "resolved" events with verified digests

[Provenance] ── consumes "resolved" + SBOM + build events ──▶
    links to commits, pipelines, packages, dependencies

## Setup
TODO
