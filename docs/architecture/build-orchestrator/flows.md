# Build Orchestrator (Cyclops): Flows and Sequences

Companion to [architecture.md](architecture.md). Each section is one flow. The
diagrams use the real topic, event and env var names from the code.

| Topic | Carries |
|---|---|
| `polar.git.repositories.events` | `GitRepositoryMessage` (rkyv), from the git observer |
| `polar.orchestrator.builds.requests` | `BuildRequest` (JSON), to the orchestrator |
| `polar.provenance.events` | `ProvenanceEvent` (rkyv from Rust agents, JSON from nushell), to the build processor |

---

## 1. CI build (git-triggered)

A ref moves on a repository listed in `repo_mappings`. The observer identifies
repos by `RepoId`, which can't be turned back into a URL. The mapping supplies the
clone URL, and also acts as the filter: unmapped repos aren't built.

```mermaid
sequenceDiagram
    autonumber
    participant GO as git observer
    participant C as Cassini
    participant O as orchestrator
    participant R as BuildRegistryActor
    participant J as BuildJobActor
    participant B as backend
    participant P as pipeline container
    participant BP as build processor

    GO->>C: RefUpdated {repo, ref_name, new} on polar.git.repositories.events
    C->>O: RefUpdated
    O->>O: find mapping where RepoId::from_url(url) == repo
    alt no mapping
        O-->>O: debug log, ignore
    else mapped
        O->>O: SourceRef::Git {url, commit_sha}.validate()
        O->>R: Insert(BuildRequest)
        O->>J: spawn
        J->>R: Transition ResolvingImage
        J->>J: image = mapping.pipeline_image
        J->>B: submit(BuildSpec)
        B-->>J: handle + graph identity (KubernetesJob {uid})
        J->>R: Transition Scheduled
        J->>C: ExecutionStarted {build_id, commit_sha, repo_url, backend}
        Note over B,P: init container clones the commit into /workspace,<br/>then the pipeline image runs (e.g. ci.nu)
        P->>C: StageStarted / SbomAnalyzed / ArtifactProduced ...
        loop every 5s until terminal
            J->>B: poll
        end
        J->>B: logs
        J->>J: upload builds/<id>/pipeline.log
        J->>C: ExecutionCompleted | ExecutionFailed
    end
    C->>BP: all ProvenanceEvents
    BP->>BP: project BuildJob, GitCommit-[:BUILT_BY]->BuildJob,<br/>BuildJob-[:EXECUTED_IN]->KubernetesJob
```

## 2. Dependency ingestion: where `archive_sha256` comes from

The analysis loop can only start from packages whose published archive has a
verified content identity. That identity comes from the *consumer's* lockfile,
through the SBOM.

```mermaid
sequenceDiagram
    autonumber
    participant CI as CI pipeline (ci.nu)
    participant CDX as cargo-cyclonedx
    participant SB as sbom.nu
    participant C as Cassini
    participant BP as build processor
    participant G as Neo4j

    CI->>CDX: generate SBOM for each workspace crate
    CDX-->>CI: components[].hashes = [{alg: SHA-256, content: Cargo.lock checksum}]
    CI->>SB: extract-graph-fragment
    SB->>SB: archive-sha256: SHA-256 hash, lowercased,<br/>null for path and git dependencies
    SB->>C: SbomAnalyzed {root, components[{purl, ..., archive_sha256}], edges}
    C->>BP: SbomAnalyzed
    BP->>G: MERGE (p:Package {purl})<br/>SET p.archive_sha256 = coalesce(p.archive_sha256, row.archive_sha256)
    BP->>G: MERGE (a)-[:DEPENDS_ON]->(b)
```

`coalesce` makes the digest first-writer-wins. A published archive is immutable,
so a later SBOM claiming a different digest for the same purl doesn't replace the
one analyses were pinned to.

## 3. Analysis run, end to end

```mermaid
sequenceDiagram
    autonumber
    participant S as analysis scheduler
    participant G as Neo4j
    participant C as Cassini
    participant O as orchestrator
    participant J as BuildJobActor
    participant B as backend
    participant I as init container
    participant A as analyzer container
    participant BP as build processor

    S->>G: IN_FLIGHT_QUERY, then PENDING_PACKAGES_QUERY (limit = free slots)
    G-->>S: [(purl, archive_sha256)]
    S->>S: crates.io URL from purl, BuildRequest {RegistryArchive, pipeline_image: analyzer@sha256}
    S->>C: BuildRequest (JSON) on polar.orchestrator.builds.requests
    C->>O: BuildRequest
    O->>O: request.validate(): https, sha256 hex, purl, image pinned by digest
    O->>J: spawn
    J->>B: submit(BuildSpec) with hardened security context
    B-->>J: handle + graph identity
    J->>C: ExecutionStarted {build_id, repo_url: download_url, commit_sha: ""}
    J->>C: PackageAnalysisStarted {build_id, purl, archive_sha256, analyzer_image}
    B->>I: start
    I->>I: download, verify sha256, extract (see section 5)
    B->>A: start (only if init exited 0)
    A->>A: analyze-manifest /workspace
    A->>C: AnalysisFindingReported {build_id, rule_id, message, path} per finding
    A-->>B: exit 0
    J->>B: poll -> Succeeded
    J->>C: ExecutionCompleted
    C->>BP: events
    BP->>G: (BuildJob)-[:ANALYZED {analyzer_image, archive_sha256}]->(Package)
    BP->>G: (BuildJob)-[:REPORTED]->(AnalysisFinding)-[:IS]->(Finding)
    BP->>G: (BuildJob)-[:TRANSITIONED_TO]->(BuildJobState {phase: succeeded})
    Note over S,G: the next scheduler pass sees the succeeded run<br/>and no longer selects this package for this image
```

The empty `commit_sha` is deliberate: an archive has no commit. The build
processor skips the `GitCommit-[:BUILT_BY]` edge when it's empty, so analysis runs
don't all collapse onto one empty-oid commit node.

## 4. Scheduler pass

```mermaid
flowchart TD
    T["Tick (every ANALYSIS_INTERVAL_SECS)"] --> D{draining?}
    D -- yes --> X[skip]
    D -- no --> F["in_flight = IN_FLIGHT_QUERY"]
    F -- "graph error" --> E["GraphOpFailed to healthcheck; retry next tick"]
    F --> L["limit = min(batch_size, max_in_flight - in_flight)"]
    L --> Z{limit = 0?}
    Z -- yes --> X2["at capacity; skip"]
    Z -- no --> Q["PENDING_PACKAGES_QUERY LIMIT limit"]
    Q -- "graph error" --> E
    Q --> EACH["for each (purl, sha256)"]
    EACH --> R{"requested within<br/>ANALYSIS_REQUEST_TTL_SECS?"}
    R -- yes --> EACH
    R -- no --> U{"crates.io URL<br/>derivable + request valid?"}
    U -- no --> W["warn once per TTL; skip"] --> EACH
    U -- yes --> P["publish BuildRequest"] --> EACH
```

A package is **due** for analyzer image `X` when all of these hold:

- its purl starts with `pkg:cargo/` and has no `?` qualifiers. Path and git
  dependencies have qualifiers, and `source` can't tell them apart: it reports
  `registry` for path crates.
- it has an `archive_sha256`.
- no run with `X` has succeeded or is still in flight.
- fewer than `ANALYSIS_MAX_FAILED_ATTEMPTS` runs with `X` have failed.

The dedup window covers the seconds between publishing a request and the
orchestrator's `PackageAnalysisStarted` reaching the graph. Losing it on restart
risks at most one duplicate request per package.

## 5. Source fetch and verification (init container)

```mermaid
flowchart TD
    S["git-clone-entrypoint"] --> K{CYCLOPS_SOURCE_KIND}
    K -- "git (or unset)" --> G1["require 40-hex CYCLOPS_COMMIT_SHA"]
    G1 --> G2["partial clone, fetch commit, checkout"]
    G2 --> G3["rm -rf .git"] --> OK["exit 0: workspace ready"]
    K -- archive --> A1{"https URL and<br/>64-hex sha256?"}
    A1 -- no --> FAIL["exit 1: job fails,<br/>main container never starts"]
    A1 -- yes --> A2["curl --proto =https to /tmp"]
    A2 --> A3{"sha256sum ==<br/>CYCLOPS_ARCHIVE_SHA256?"}
    A3 -- no --> FAIL
    A3 -- yes --> A4["tar -xz --strip-components=1<br/>--no-same-owner --no-same-permissions"]
    A4 --> OK
    K -- other --> FAIL
```

Nothing from an unverified archive ever reaches `/workspace`. Stripping one
leading directory fits the crates.io, npm and PyPI source-archive layout, so
`/workspace` is the package root. Zip archives such as wheels aren't supported.

## 6. Job lifecycle and polling

```mermaid
sequenceDiagram
    autonumber
    participant J as BuildJobActor
    participant R as BuildRegistryActor
    participant B as backend
    participant S as object storage
    participant C as Cassini

    J->>R: Transition ResolvingImage
    alt no image in request or repo mapping
        J->>R: Transition Failed
        J->>C: ExecutionFailed {stage: resolving_image}
    else image resolved
        J->>B: submit
        alt submission error
            J->>R: Transition Failed
            J->>C: ExecutionFailed {stage: scheduling}
        else submitted
            J->>R: Transition Scheduled
            J->>C: ExecutionStarted (+ PackageAnalysisStarted for archives)
            loop every 5s
                J->>B: poll
                alt Pending
                    J->>J: keep polling
                else Running
                    J->>R: Transition Running
                else Succeeded
                    J->>B: logs
                    J->>S: put builds/<id>/pipeline.log (best effort)
                    J->>R: Transition Succeeded
                    J->>C: ExecutionCompleted {duration_secs}
                else Failed {reason}
                    J->>B: logs
                    J->>S: put pipeline.log (best effort)
                    J->>R: Transition Failed
                    J->>C: ExecutionFailed {stage: execution}
                else Unknown (backend lost the job)
                    J->>R: Transition Failed
                    J->>C: ExecutionFailed {stage: execution}
                else poll error
                    J->>J: retry indefinitely (no retry budget yet)
                end
            end
        end
    end
```

A failure to upload logs never changes a build's outcome. `BuildJobMessage::Cancel`
exists, along with its `ExecutionCancelled` event, but nothing sends it yet.

## 7. Retry and eligibility, per (package, analyzer image)

How the graph state of a package's runs with one image decides whether the
scheduler requests it again:

```mermaid
stateDiagram-v2
    [*] --> Due: package has archive_sha256
    Due --> Requested: scheduler publishes BuildRequest
    Requested --> InFlight: PackageAnalysisStarted projected
    Requested --> Due: TTL expires with no run (request rejected or lost)
    InFlight --> Done: run succeeded
    InFlight --> Due: run failed or cancelled, failures < max
    InFlight --> GaveUp: failures reach ANALYSIS_MAX_FAILED_ATTEMPTS
    InFlight --> Stuck: job died with no terminal event
    Done --> [*]
    GaveUp --> [*]
```

`Stuck` is a known gap. A run with no `completed_at` counts as in flight forever,
until the orchestrator gets reconciliation (`Unreconciled`). A new analyzer image
digest puts every package back into `Due` for that image.

## 8. Health, rejuvenation and drain

Both the orchestrator and the scheduler follow the standard Polar healthcheck
pattern.

```mermaid
sequenceDiagram
    autonumber
    participant SUP as supervisor (orchestrator or scheduler)
    participant HC as HealthCheckActor
    participant C as Cassini client
    participant K as kubelet

    C-->>SUP: Registered
    SUP->>HC: CassiniConnected
    Note over SUP,HC: the scheduler also sends GraphConnected / GraphAvailable / GraphOpFailed<br/>from its real queries
    C-->>SUP: TransportError
    SUP->>HC: CassiniDisconnected (non-fatal: in-flight builds keep polling)
    HC->>SUP: PrepareShutdown (cert near expiry or dependency unhealthy)
    SUP->>HC: ShutdownAck
    HC-->>SUP: HealthCheckActor terminates
    SUP->>SUP: draining = true, ForceExit after 5s
    Note over SUP: while draining, new messages are logged, not dispatched
    SUP->>SUP: ForceExit: process::exit(1)
    K->>SUP: restart
```

Restarting the orchestrator loses its in-memory record of running jobs. The jobs
themselves keep running on the backend and still emit their own events, but
their terminal lifecycle event is never sent: this is the `Stuck` case above.
