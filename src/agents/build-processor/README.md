# Build Processor

The build processor writes Polar's software supply-chain facts into the
knowledge graph: builds, what they produced, findings about packages, and what
registries actually serve. Every agent that reports such a fact publishes a
canonical [`ProvenanceEvent`] to `polar.provenance.events`. This agent is the
one that records it.

## Design

- **The single writer.** No other agent writes these node types. Agents report
  facts; this one projects them.
- **Projection is pure.** `project::project(event, observed_at)` returns the
  `Vec<GraphOp>` that records the event. Nothing in `project/` touches the
  graph or the broker. That makes every projection unit-testable and runnable
  against a real Neo4j as-is.
- **Every write is an idempotent MERGE.** Replaying the broker log reproduces
  the same graph. Events about the same nodes may arrive in any order.
  Timestamped edges are MERGEd on their endpoints alone, with `at` set only on
  creation, so replays never duplicate them.
- **One dispatch, no wildcard.** `project()` matches every `ProvenanceEvent`
  variant explicitly. A new variant doesn't compile until it's routed, and
  variants with no projection yet warn instead of disappearing.
- **One mailbox.** The supervisor decodes each event, projects it, and sends the
  operations in order to the `GraphControllerActor`, which serializes all
  writes. There are no per-domain actors: projection holds no state, so they
  would add a hop and split ordering without buying isolation.

## Modules

The projections are grouped by what the graph is *about*, which also tracks
whose word each fact is.

| Module | Subject | Reported by | Events |
|---|---|---|---|
| `project/execution.rs` | `BuildJob`: what ran, where, how it ended | orchestrator, CI | `ExecutionStarted`, `StageStarted`, `StageCompleted`, `ExecutionCompleted`, `ExecutionFailed`, `ExecutionCancelled`, `PackageAnalysisStarted` |
| `project/outputs.rs` | what a build produced | the producing build | `ArtifactProduced`, `SbomAnalyzed`, `BinaryLinked`, `ContainerImageCreated` |
| `project/findings.rs` | facts about packages | scanners, analyzers | `SecurityAdvisoryFound`, `PackageStatusFound`, `AnalysisFindingReported` |
| `project/registry.rs` | what a registry actually serves | the resolver | `OCIArtifactResolved`, `OCIRegistryDiscovered` |

Not projected:
- `OCIArtifactDiscovered` and `ArtifactDiscovered` are requests for the resolver. Only the resolved result is recorded.
- `OCIArtifactCreated` and `ImageRefResolved` have no producer yet. They warn when seen.

## Graph

```text
execution  (GitCommit)-[:BUILT_BY]->(BuildJob)                     git sources only
           (BuildJob)-[:EXECUTED_IN]->(KubernetesJob | PodmanPod)
           (BuildJob)-[:IS]->(BuildExecution)
           (BuildJob)-[:HAS_STAGE]->(BuildStage)
           (BuildJob)-[:TRANSITIONED_TO]->(BuildJobState {phase})-[:OF_TYPE]->(State)
           (BuildJob)-[:ANALYZED {analyzer_image, archive_sha256}]->(Package)

outputs    (BuildJob)-[:PRODUCED]->(BuildArtifact)
           (BuildArtifact)-[:ANALYZED_AS]->(Sbom)-[:DESCRIBES]->(Package)
           (Package)-[:DEPENDS_ON]->(Package)
           (Binary)-[:BUILT_FROM]->(Package),  (Sbom)-[:ATTESTS]->(Binary)
           (BuildArtifact)-[:INSTANCE_OF]->(OCIArtifact)            pushed images
           (OCIArtifact)-[:HAS_DIFF_ID {order}]->(OCILayerDiffId)

findings   (BuildJob)-[:REPORTED]->(SecurityAdvisory | PackageStatus | AnalysisFinding)-[:IS]->(Finding)
           (SecurityAdvisory)-[:AFFECTS]->(Package),  (PackageStatus)-[:CONCERNS]->(Package)

registry   (OCIArtifact)-[:HOSTED_BY]->(OCIRegistry)
           (OCIArtifact)-[:HAS_LAYER {order}]->(OCILayer)
           (OCIArtifact)-[:HAS_MANIFEST {platform_*}]->(OCIArtifact)    indexes
```

Points that are easy to get wrong:

- **`BuildJob.completed_at` means finished, not succeeded.** Success is the
  latest `BuildJobState {phase: 'succeeded'}`.
- **An image has two layer identities.** `OCILayer` is a registry blob, keyed
  by compressed digest. `OCILayerDiffId` is the image config's uncompressed
  diff ID, which SBOM tools reference. Both hang off the same
  `OCIArtifact {digest}`, and matching `order` pairs them.
- **Images meet on `OCIArtifact {digest}`.** A pushed build image, the
  resolver's manifest for it, and k8s's `PodContainer-[:USES_IMAGE]` (written
  by the kube consumer) all land on this node. Whether those claims agree is a
  graph query.
- **`Package.archive_sha256` is first-writer-wins.** A published archive is
  immutable, so a conflicting later SBOM doesn't replace it.

Not owned here and only referenced through edges: `GitCommit` (git
processor), backend job nodes (their observers), `PodContainer` (kube
consumer).

## Topics

| Topic | Purpose |
|---|---|
| `polar.provenance.events` | All provenance events: rkyv from Rust agents, JSON from nushell stages |
| `polar.provenance.linker` | Legacy. The GitLab consumer still publishes `OCIRegistryDiscovered` here |

## Setup

```
GRAPH_DB        — Neo4j database name
GRAPH_USER      — Neo4j username
GRAPH_PASSWORD  — Neo4j password
GRAPH_ENDPOINT  — Bolt endpoint, e.g. bolt+s://neo4j.polar.svc.cluster.local:7687
```

Optional mTLS (all three or none): `GRAPH_CLIENT_CERT`, `GRAPH_CLIENT_KEY`,
`GRAPH_CA_CERT`. The broker connection uses the standard `cassini-client`
environment.

## Tests

```sh
cargo test -p build-processor

# Every projection against a real Neo4j 5.26.2 (needs a container runtime):
DOCKER_HOST=unix://<podman socket> TESTCONTAINERS_RYUK_DISABLED=true \
  cargo test -p build-processor --test projection_neo4j -- --ignored
```
