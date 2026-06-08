# Build Processor

The **Build Processor** is the unified graph projection agent for Polar's software supply chain provenance system. It consumes canonical [`ProvenanceEvent`] instances from the Cassini broker and projects them into the Neo4j knowledge graph.

## Responsibilities

This agent owns two domains of graph projection:

**Build execution lifecycle** — projects `BuildJob`, `BuildStage`, and `Vulnerability` nodes from CI pipeline events. Establishes the `BUILT_BY` edge from `GitCommit` to `BuildJob`, `HAS_STAGE` edges to `BuildStage` nodes, and `FOUND_VULNERABILITY` edges to scanner-reported CVEs.

**Artifact provenance** — projects `Sbom`, `Package`, `Binary`, `ContainerImage`, `OCILayer`, `BuildArtifact`, and `Artifact` nodes from pipeline artifact events. Establishes the full provenance chain:
```
(Binary)-[:BUILT_FROM]->(Package)<-[:DESCRIBES]-(Sbom)
(ContainerImage)-[:HAS_LAYER]->(OCILayer)
(BuildArtifact)-[:ANALYZED_AS]->(Sbom)
(Sbom)-[:ATTESTS]->(Binary)
```

## Design Philosophy

**Stateless projection** — each event is projected independently. Operations use Cypher MERGE semantics and are idempotent: replaying events from the broker produces the same graph state. The broker log is the source of truth; the graph can always be rebuilt.

**Single topic, unified vocabulary** — all agents publish [`ProvenanceEvent`] instances to `polar.provenance.events`. The build processor subscribes to one topic and sees one type regardless of whether the event originated from a CI pipeline stage, the k8s observer, the GitLab observer, or the resolver.

**Two wire formats, one type** — Rust agents serialize `ProvenanceEvent` directly via rkyv. Nushell pipeline stages emit JSON matching variant shapes. Both paths converge at `ProvenanceEvent` before dispatch. No intermediary translation layer.

**Node ownership** — `BuildJob`, `BuildStage`, `Vulnerability`, `Sbom`, `Package`, `Binary`, `ContainerImage`, `OCILayer`, `BuildArtifact` are owned by this processor and freely upserted. `GitCommit` and `BackendJob` are foreign — referenced only via `EnsureEdge`. The authoritative agent owns their properties.

## Actor Tree

```
BuildProcessorSupervisor
  ├── TcpClient              — Cassini broker connection
  ├── GraphControllerActor   — Neo4j connection pool, serializes graph writes
  ├── BuildActor             — execution lifecycle projection (project_event)
  └── ProvenanceLinker       — artifact domain projection (SBOM, Binary, OCI)
```

The supervisor deserializes incoming messages and routes by variant — lifecycle events go to `BuildActor`, artifact domain events go to `ProvenanceLinker`. Neither child actor blocks on the other; both cast graph operations to the shared `GraphControllerActor` which serializes writes into the Neo4j connection pool.

## Event Routing

| Variant | Handler |
|---|---|
| `ExecutionStarted`, `StageStarted`, `StageCompleted` | `BuildActor` → `project_event` |
| `ExecutionCompleted`, `ExecutionFailed`, `ExecutionCancelled` | `BuildActor` → `project_event` |
| `VulnerabilityFound` | `BuildActor` → `project_event` |
| `ArtifactProduced`, `SbomAnalyzed`, `BinaryLinked` | `ProvenanceLinker` |
| `ContainerImageCreated`, `OCIArtifactResolved` | `ProvenanceLinker` |
| `ImageRefResolved`, `OCIRegistryDiscovered` | `ProvenanceLinker` |
| `PodContainerUsesImage` | `ProvenanceLinker` |
| `OCIArtifactDiscovered`, `ImageRefDiscovered` | logged and discarded — resolver handles these |

## Topics

| Topic | Purpose |
|---|---|
| `polar.provenance.events` | Primary — all canonical provenance events |

## Graph Schema

Nodes projected by this agent:

| Label | Key | Owner |
|---|---|---|
| `BuildJob` | `build_id` | This processor |
| `BuildJobState` | `(build_id, valid_from)` | This processor |
| `BuildStage` | `(build_id, stage_id)` | This processor |
| `BuildArtifact` | `digest` | This processor |
| `Vulnerability` | `identifier` | This processor |
| `Sbom` | `artifact_content_hash` | This processor |
| `Package` | `purl` | This processor |
| `Binary` | `content_hash` | This processor |
| `ContainerImage` | `config_digest` | This processor |
| `OCILayer` | `digest` | This processor |
| `GitCommit` | `oid` | VCS agent (referenced only) |
| `BackendJob` | varies by backend | Backend observer (referenced only) |

## Setup

Requires the following environment variables:

```
GRAPH_DB        — Neo4j database name
GRAPH_USER      — Neo4j username
GRAPH_PASSWORD  — Neo4j password
GRAPH_ENDPOINT  — Bolt endpoint, e.g. bolt+s://neo4j.polar.svc.cluster.local:7687
```

Optional mTLS (all three required together or not at all):

```
GRAPH_CLIENT_CERT — path to client certificate
GRAPH_CLIENT_KEY  — path to client private key
GRAPH_CA_CERT     — path to CA certificate
```

Cassini broker connection is configured via the standard `cassini-client` environment variables.

Once environment is set, build and run with standard `cargo` commands.
