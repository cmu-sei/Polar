# Cyclops — Polar's Authoritative Builder

Cyclops runs containerized jobs against source code that is pinned and verified,
and reports what happened as provenance events in Polar's knowledge graph.

It's authoritative in a narrow sense. The orchestrator, not the job, reports which
image ran, against which source, on which backend, and how it ended. A job reports
only what it alone can know, such as its findings. A compromised or buggy job
can't misreport its own inputs. This is the "authoritative builder" role in what
the Polar team calls the enforcement kernel.

Cyclops does two kinds of work with the same machinery:

| | CI builds | Dependency source analysis |
|---|---|---|
| **Source** | A git repository at a commit SHA | A published package archive (e.g. a `.crate`) verified against its sha256 |
| **Triggered by** | The git observer's `RefUpdated`, for repos in `repo_mappings` | The analysis scheduler, from a graph query |
| **Image** | The repo's pipeline image (e.g. `polar-ci` running `ci.nu`) | A hardened analyzer image, pinned by digest |
| **Runtime posture** | Image's own user and capabilities | Non-root, read-only root, all capabilities dropped |
| **Records** | `(GitCommit)-[:BUILT_BY]->(BuildJob)` | `(BuildJob)-[:ANALYZED]->(Package)`, `(BuildJob)-[:REPORTED]->(AnalysisFinding)` |

Analysis is the main reason Cyclops was revived. The build processor already
ingests a project's full dependency tree from its SBOMs. Cyclops then runs
standardized analysis on each dependency's *published source*: the code that
actually gets compiled into your artifacts, which isn't necessarily what's in the
dependency's declared git repository. The graph records which analyzer has
analyzed which package, so the graph itself is the work queue and the record of
past runs.

Detailed architecture, sequence diagrams and the graph model:
[`docs/architecture/build-orchestrator/`](../../../docs/architecture/build-orchestrator/architecture.md).

## Components

| Path | Crate / artifact | Role |
|---|---|---|
| `core/` | `orchestrator-core` | Domain types: `SourceRef`, `BuildRequest`, `BuildSpec`, `BuildState`, the `BuildBackend` trait |
| `agent/` | `build-orchestrator` (binary) | The orchestrator: supervisor, build registry, one `BuildJobActor` per job |
| `k8s/` | `orchestrator-backend-k8s` | Runs jobs as Kubernetes `Job`s |
| `podman/` | `orchestrator-backend-podman` | Runs jobs as rootless Podman pods over the libpod socket |
| `scheduler/` | `analysis-scheduler` (binary) | Turns "packages not yet analyzed by image X" into build requests |
| `package.nix` → `cloneImage` | `cyclops/git-clone` | Init container: clones a commit, or downloads and verifies an archive |
| `../../analyzers/cargo-manifest/` | `polar-analyzer-cargo-manifest` | First analyzer image: build scripts, proc-macros, native `links` |

Flake outputs: `buildOrchestratorImage`, `analysisSchedulerImage`, `cloneImage`,
`cargoManifestAnalyzerImage`.

## How a job runs

1. **A request arrives.**
   - A git `RefUpdated` on a mapped repo becomes a request with `SourceRef::Git`.
   - A JSON `BuildRequest` on `polar.orchestrator.builds.requests` is taken as-is.
   - Either way it's validated. SHAs must be full lowercase hex. Archives must be `https` with a sha256 digest. Archive requests must name an image pinned by `@sha256:`.
2. **The image is resolved.** It's the request's `pipeline_image`, or otherwise the repo mapping's image. If neither exists the build fails cleanly; bootstrapping a missing image isn't implemented.
3. **The job is submitted.** The init container populates `/workspace`:
   - git: clone and check out the exact commit.
   - archive: download over HTTPS, verify the sha256 *before* extracting, then unpack.

   Then the job's container runs. It gets `POLAR_BUILD_ID`, the source identity, and the configured `job_env` (`BROKER_ADDR`, `CASSINI_SERVER_NAME`).
4. **Cyclops reports what's running.** It emits `ExecutionStarted` with the backend job's graph identity. For archives it also emits `PackageAnalysisStarted` (purl, verified digest, analyzer image).
5. **Cyclops polls to a terminal state** every 5s, uploads the pipeline log to object storage, and emits `ExecutionCompleted`, `ExecutionFailed` or `ExecutionCancelled`.

Build state machine (in-memory, per orchestrator instance):

```
Pending → ResolvingImage → Scheduled → Running → Succeeded | Failed
Cancelled: from any non-terminal state
Bootstrapping, Unreconciled: defined, not implemented
```

## Topics

| Topic | Direction | Payload |
|---|---|---|
| `polar.git.repositories.events` | in | rkyv `GitRepositoryMessage` (git observer); only `RefUpdated` triggers builds |
| `polar.orchestrator.builds.requests` | in | JSON `BuildRequest` (analysis scheduler, or anything else) |
| `polar.provenance.events` | out | rkyv `ProvenanceEvent`: lifecycle events and `PackageAnalysisStarted`. Analyzers publish `AnalysisFindingReported` here as JSON |

## Configuration

### Orchestrator

YAML (rendered from `agent/conf/*.dhall`), read from `ORCHESTRATOR_CONFIG_FILE`
(default `cyclops.yaml`). Any key can be overridden with `CYCLOPS__` env vars,
using `__` as the separator.

```yaml
backend:
  driver: kubernetes            # or podman
  command: []                   # empty = run the image's own entrypoint
  job_env:                      # injected into every job container; a list,
                                # because the config loader lowercases map keys
    - name: BROKER_ADDR
      value: cassini-ip-svc.polar.svc.cluster.local:8080
    - name: CASSINI_SERVER_NAME
      value: cassini-ip-svc.polar.svc.cluster.local
  kubernetes:
    namespace: polar-builds
    ca_bundle_secret: null      # Secret whose ca-bundle.crt replaces job containers'
                                # trust store; set it behind a TLS-inspecting proxy
  podman: null                  # or { socket_path, image_pull_policy }
repo_mappings:                  # git repos Cyclops builds, and with what image
  - repo_url: https://github.com/cmu-sei/Polar.git
    pipeline_image: registry.example/polar-ci@sha256:...
storage:                        # pipeline logs: builds/<build_id>/pipeline.log
  endpoint_url: http://object-store.cyclops-dev.svc.cluster.local:9000
  bucket: cyclops-build-artifacts
  access_key: ...
  secret_key: ...
```

### Analysis scheduler

Environment variables:

| Variable | Default | Meaning |
|---|---|---|
| `ANALYZER_IMAGE` | (required) | Analyzer image, must be pinned `…@sha256:…`. Its digest is the analyzer's identity in the graph |
| `ANALYSIS_INTERVAL_SECS` | `60` | Time between scheduling passes |
| `ANALYSIS_BATCH_SIZE` | `10` | Maximum requests per pass |
| `ANALYSIS_MAX_IN_FLIGHT` | `20` | Maximum unfinished runs of this analyzer |
| `ANALYSIS_MAX_FAILED_ATTEMPTS` | `3` | Stop requesting a package after this many failures |
| `ANALYSIS_REQUEST_TTL_SECS` | `900` | Suppress re-requesting a package this long after publishing |

Plus the usual Polar graph and broker settings (`get_neo_config`, Cassini TLS env).

## Security posture for analysis jobs

Archive sources are third-party code and are treated as hostile.

- **The source is verified before it reaches the workspace.** The init container rejects anything that isn't `https`, and checks the sha256 before extraction. It extracts without keeping the archive's ownership or permissions.
- **The analyzer image is minimal.** It's built with `nix-container-lib` in Minimal mode on the Micro layer: no shell, no Nix, no toolchain, UID 65532, and a pinned prelude. Its digest should change only when the analysis does.
- **Both containers are locked down at runtime.** On k8s: non-root, read-only root filesystem, no privilege escalation, all capabilities dropped, `RuntimeDefault` seccomp, and a 256Mi `/tmp` as the only writable path. On Podman: the libpod equivalents.
- **Git credentials are never given to archive jobs**, and no service account token is mounted.
- **Not yet restricted: network egress.** That needs a NetworkPolicy. The analyzer only needs the broker, and the init container only needs the archive host.

## Running locally

The orchestrator runs on your machine; jobs run on a local Kubernetes cluster
(written against OrbStack, any cluster whose nodes can run locally loaded
images works). Images are Linux images built with Nix; on macOS build them in a
Linux machine (on OrbStack, `orb -m <nixos machine> nix build ...`) and
`docker load` them into the cluster's engine.

1. **Dev infrastructure**, from `deploy/dev.dhall`: the object store and the
   job namespace.
   ```sh
   kubectl apply -f deploy/dev.yaml -l 'app.kubernetes.io/name in (cyclops-dev,object-store,cyclops-jobs)'
   cargo test -p build-orchestrator --test object_store -- --ignored   # checks the store
   ```
   - **Object store** (versitygw): `http://localhost:30900` from the host,
     `http://object-store.cyclops-dev.svc.cluster.local:9000` in the cluster.
     Credentials `cyclops-dev` / `cyclops-dev-secret`, bucket
     `cyclops-build-artifacts`.
   - **`cyclops-jobs`**, where jobs run. Its `ResourceQuota` (4 pods, 2/4 CPU,
     2/6 GiB requests/limits) and `LimitRange` (at most 1 CPU / 2 GiB per
     container) are a hard backstop: nothing that publishes requests can swamp
     the laptop. A pod over the limit range is refused, and the orchestrator
     fails that build at once with Kubernetes' reason; a pod over the quota
     waits for room.
2. **Certificates.** `nix build .#tlsCerts -o result-tls-certs` gives a dev CA
   and Cassini server and client certs (server name `cassini`).
3. **Secrets in `cyclops-jobs`:** the client cert jobs mount, and, behind a
   TLS-inspecting proxy, a CA bundle that includes the public roots:
   ```sh
   kubectl -n cyclops-jobs create secret generic client-tls \
     --from-file=tls.crt=<client cert> --from-file=tls.key=<client key> --from-file=ca.crt=<ca cert>
   kubectl -n cyclops-jobs create secret generic proxy-ca \
     --from-file=ca-bundle.crt=/etc/nix/macos-keychain.crt     # macOS + Zscaler, for example
   ```
4. **Run it** with the laptop config, `agent/conf/cyclops-dev.dhall`: the
   `cyclops-jobs` namespace, job resources inside its limit range, a 1 GiB
   workspace, the dev store, and the broker on the host.
   ```sh
   dhall-to-yaml --file agent/conf/cyclops-dev.dhall --output cyclops.yaml
   ORCHESTRATOR_CONFIG_FILE=cyclops.yaml \
   TLS_CLIENT_CERT=... TLS_CLIENT_KEY=... TLS_CA_CERT=... \
   BROKER_ADDR=127.0.0.1:8080 CASSINI_SERVER_NAME=cassini \
   CYCLOPS_DEV_MODE=1 cargo run -p build-orchestrator
   ```
   Any setting can be overridden from the environment, e.g. behind a proxy
   `CYCLOPS__BACKEND__KUBERNETES__CA_BUNDLE_SECRET=proxy-ca`.
   `CYCLOPS_DEV_MODE=1` injects a `RefUpdated` for the first repo mapping, so a
   git build runs without the git observer.
5. **Analysis on a small dependency set.** Seed the graph with the toy crate's
   ~15 dependencies (`src/analyzers/fixtures/`), not Polar's ~770, and run the
   scheduler with laptop-sized settings:
   ```sh
   nu src/analyzers/fixtures/seed-toy-sbom.nu
   ANALYZER_IMAGE=polar-analyzer-cargo-manifest@sha256:... \
   ANALYSIS_MAX_IN_FLIGHT=2 ANALYSIS_BATCH_SIZE=2 ANALYSIS_INTERVAL_SECS=120 \
   cargo run -p analysis-scheduler
   ```
   Both also need the broker environment (as in step 4); the scheduler needs
   the graph too (`GRAPH_ENDPOINT`, `GRAPH_USER`, `GRAPH_PASSWORD`, `GRAPH_DB`).
   With OrbStack's containerd image store, a `docker load`ed image is
   addressable by digest (`docker image inspect --format '{{.Id}}'`), so the
   pinned analyzer image needs no registry. The fixtures README lists the
   findings a complete run should produce.

The orchestrator needs a Cassini broker. Its broker client gives up after
about 9 s of failed connects; the orchestrator then drains and exits (status
1), like the other agents, so a platform restarts it. Without a broker, a dev
run therefore lasts about 15 s: enough for a quick build, but its events are
not delivered.

Run an analyzer against an extracted crate (needs `cassini-client` on PATH):

```sh
POLAR_BUILD_ID=dev BUILD_WORKSPACE=/path/to/crate \
  nu src/analyzers/cargo-manifest/analyze.nu
```

Tests:

```sh
cargo test -p build-orchestrator -p orchestrator-core \
  -p orchestrator-backend-k8s -p orchestrator-backend-podman -p analysis-scheduler

# Against a real Neo4j 5.26.2 (needs a container runtime); with Podman:
DOCKER_HOST=unix://<podman socket> TESTCONTAINERS_RYUK_DISABLED=true \
  cargo test -p analysis-scheduler --test pending_query -- --ignored
```

## Status and known gaps

Both paths have run end to end on a local cluster (OrbStack), through a real
broker, build processor and Neo4j:

- **Git build:** orchestrator to Kubernetes Job, clone at the exact commit,
  pipeline container, log in the object store, `BuildJob` in the graph linked
  to its commit and to the real Kubernetes Job.
- **Analysis loop:** the toy crate's SBOM seeded through the pipeline library,
  the scheduler fanning out from the graph (at most 2 in flight), 15 verified
  archives analyzed, and the 8 expected findings in the graph. The next pass
  found nothing left to do.

Nothing has run at the scale of Polar's own dependency tree, or in a deployed
cluster. Known gaps:

- **Job state is in memory only.** A job that dies without a terminal event looks in flight forever, so the scheduler won't re-request it. Fixing this means implementing the `Unreconciled` state.
- **The orchestrator has no concurrency limit of its own.** The scheduler's in-flight cap is the throttle for analysis, and in dev the `cyclops-jobs` quota is the backstop. Requests published by hand are unthrottled up to that quota.
- **No crates.io cache.** Every analysis run downloads its archive.
- **Git credentials are placeholders**; a colleague owns the credentials design. Missing pipeline images aren't bootstrapped.
- **The `build_job_actor` tests are disabled.** Their mock publisher predates the current `CassiniClient` trait.
- **`infra/` has no deployment values** for the orchestrator, scheduler or analyzers.
