# Provenance Agent

The **Provenance Agent** is a stateless background service responsible for tracing the origin and lifecycle of software artifacts within the graph database. It analyzes existing data in Neo4j and constructs relationships between previously disconnected entities such as Pods, Container Images, Pipelines, Commits, and Charts.


## Design Philosophy

- **Passive analysis**: does not listen to real-time events; instead runs on a cadence set by operators
- **Query-driven**: leverages Cypher queries to discover relationships from current graph state
- **Decoupled**: does not interact with other services.
- **Idempotent**: safe to run repeatedly; avoids creating duplicate relationships


## Provenance and the ImageReference Model

### Problem Statement

Software provenance is a foundational concern in Polar’s observability model. We want to be able to trace the lifecycle of an artifact — from its creation in source control, through CI pipelines, to its presence in container registries, and finally to its deployment in a cluster or runtime environment.

The practical challenge lies in the fact that each of these systems (GitLab, container registries, Kubernetes, etc.) exposes only partial and siloed metadata. A GitLab pipeline knows the image tag it produced, but not necessarily its digest after push. The registry knows the digest but doesn’t know which commit built it. Kubernetes knows what image was deployed, but not who built it or when.

Polar’s goal is to unify those perspectives into a single graph where relationships between these artifacts can be queried, verified, and audited without relying on manual correlation.

---

### Stories We Want to Tell

The provenance model should allow us to answer and visualize stories like:

* **“Where did this running image come from?”**
  → Trace a container running in Kubernetes back to the registry tag, to the pipeline that built it, to the Git commit that triggered that build.

* **“Which deployments include unverified or unscanned images?”**
  → Identify deployments whose image digests don’t correspond to any signed or scanned artifact.

* **“Who or what built this image?”**
  → Link from the image to the CI job, project, and user that produced it.

* **“Has this tag ever changed digest?”**
  → Reveal retags or overwritten images by comparing historical registry state.

All of these require a **stable reference model** capable of linking semantically identical data across heterogeneous sources.

---

### Initial Approach and Limitations

Early in Polar’s design, observer agents were built to scrape each system’s API and ship raw observations via the Cassini broker to consumer agents, which executed Cypher queries to insert those facts into the graph.

For example, the GitLab observer captured build metadata and image tags; the ACR observer captured repository digests and timestamps; the Kubernetes observer captured the currently deployed image strings.

Each of these agents populated its own node types, such as `GitLabPipeline`, `ContainerImageTag`, `ACRImage`, or `KubeDeployment`. But without a shared key or reference, the graph contained parallel but disconnected entities — each describing its own fragment of the same real-world artifact.

In short: we could see everything, but not connect anything.

---

### Introducing the `ImageReference` Abstraction

The solution was to introduce a **canonical reference node** — `ImageReference` — representing the normalized identity of a container image, independent of where it appears.

An `ImageReference` is not specific to any system. It captures the minimal identity that defines an image in the ecosystem:

```
<registry>/<repository>:<tag>
```

Optionally with a digest:

```
<registry>/<repository>@<digest>
```

This reference becomes the **join key** between systems. All consumers now create (or link to) an `ImageReference` node as part of their normal ingestion flow.

Example Cypher emitted by a consumer agent:

```cypher
MERGE (img:ContainerImageTag {
  registry: $registry,
  repository: $repository,
  tag: $tag
})
SET img.digest = $digest

MERGE (ref:ImageReference {
  normalized: toLower($registry + '/' + $repository + ':' + $tag)
})
MERGE (img)-[:IDENTIFIES]->(ref)
```

If another agent — say the gitlab consumer — ingests the same reference, the graph automatically merges them through the shared `ImageReference`. No orchestration or central coordination is required.

---

### Architectural Impact

This change does not alter the responsibilities of the existing agents:

* **Observer agents** continue to scrape upstream APIs and emit raw observations.
* **Consumers** continue to write Cypher, but now include `ImageReference` creation or linkage.
* **Cassini** remains the message broker and doesn’t need special routing.

The addition of the `ImageReference` node type allows Polar to scale provenance correlation horizontally: each agent remains dumb and stateless, while the graph becomes the system of record that naturally merges data across domains.

---

### The Role of the Provenance Agent

Once these reference nodes exist, the **Provenance Agent** becomes a higher-order reconciling process. Its responsibilities include:

* **Linking equivalent entities**
  For instance:

  ```cypher
  MATCH (a:ContainerImageTag)-[:IDENTIFIES]->(ref:ImageReference)<-[:IDENTIFIES]-(b:ACRImage)
  MERGE (a)-[:SAME_AS]->(b)
  ```

* **Resolving build-to-deploy lineage**

  ```cypher
  MATCH (p:PipelineRun)-[:PRODUCED]->(img:ContainerImageTag)
  MATCH (img)-[:SAME_AS]->(acr:ACRImage)
  MERGE (p)-[:RESULTED_IN]->(acr)
  ```

* **Detecting missing or orphaned links**
  Images known to the registry but absent in any pipeline history, or Kubernetes deployments referring to unknown digests.

Importantly, this reconciliation step no longer depends on an external orchestration layer or JSON schema mediation. The intelligence is embedded in the graph: Cypher’s `MERGE` ensures all partial facts converge on the same canonical nodes.

---

### Normalization and Determinism

To ensure consistent identity across agents, each image reference should be deterministically normalized. A shared utility (e.g., `polar-normalize`) can compute:

* A lowercase canonical string `<registry>/<repository>:<tag>`
* A UUIDv5 derived from that string (optional, for cross-database consistency)

This allows references to be generated or reconciled deterministically without querying the graph.

---

### Outcome

By introducing `ImageReference` as the universal intermediary, Polar can now form provenance chains across arbitrary data sources without custom glue logic.

This design keeps individual agents simple, promotes emergent linkage through data normalization, and supports incremental enrichment — any new observer that emits an `ImageReference` will automatically join the provenance network.

The result is a composable, system-agnostic foundation for answering questions like:

* “What commit built the image running in production?”
* “Which artifacts were deployed from unreviewed code?”
* “Where are all instances of image `foo:latest` currently running?”

This is the foundation of Polar’s provenance graph — not a static snapshot of systems, but a continuously converging knowledge model.


### Setup

Setup for this agent is similar that of a consumer, and thus needs to be provided the same environment variables for credentials and possible CA certificates. Once those are set, you can run your typical `cargo` commands to build and run the agent.
