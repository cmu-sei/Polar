# build-orchestrator

build-orchestrator is Polar's authoritative builder: it runs a repository's pipeline as a Kubernetes Job against pinned
source and reports what ran as provenance events. This is the orchestrator as an agent built on `polar-agent-builder`.
It was written in public Polar (`src/fork/build-orchestrator`, as a workspace of its own, since the fork's `polar`
library and the public one share a name) and is a member of this workspace now.

| | Ported from public Polar's `src/agents/build-orchestrator` |
|---|---|
| `core` | domain types and the backend trait, unchanged |
| `k8s` | the Kubernetes backend (jobs), unchanged for now |
| `agent` | the orchestrator: a `PolarAgent` (the builder owns the broker, certificates, health and shutdown); the repositories it builds are the signed runtime section `mappings` |

Not carried: the Podman backend, and dependency analysis (the analysis scheduler, and the events and graph
nodes it needs, which the fork's library does not have yet: `PackageAnalysisStarted` and `AnalysisFindingReported`
are in its vocabulary now, so the second half exists).

## Building

The builder depends on the Linux kernel keyring, so it builds on Linux only. From `src/agents`, in the fork's
development shell (on a Mac, in the Linux VM, `orb -m nix-builder`):

```sh
cargo test -p build-orchestrator -p orchestrator-core -p orchestrator-backend-k8s
```

`build-orchestrator --scaffold` prints the settings and the `mappings` section.

The object store client is `aws-sdk-s3` over rustls with ring, not the SDK's default, which is aws-lc-rs: the fork
builds neither aws-lc nor its C toolchain (`src/agents/Cargo.toml`, `build-orchestrator/agent/src/client.rs`).
`tests/object_store.rs` needs an S3-compatible store and is ignored without one.

## Images

Two, built with nix (`workspace.nix`, `flake.nix`):

- `.#buildOrchestratorImage`: the `build-orchestrator` binary and `polar-healthcheck`, which the liveness probe runs
  (`package.nix`, `container.dhall`, rendered to `container.nix`).
- `.#buildOrchestratorObjectStoreImage`: an S3-compatible store for the jobs' logs (the Versity gateway over a
  volume; `object-store-image.nix`). On a cloud a bucket replaces it.

## Deploying

`types/build-orchestrator.dhall` in the deployment library is the stage: the agent, its Role in the jobs' namespace, the
store, and the secret that opens it.
Every build's pod runs as one ServiceAccount, `build-job`, a workload with many instances (`backend.kubernetes.account`): each
pod has a certificate and a session of its own, naming the pod, and `max_concurrent_builds` is the only limit. A deployment that has
not marked its workload can still give a fixed set (`backend.kubernetes.accounts`, one running job to each); the two are refused together. Blitz's plan includes it after the git agents.

## What it builds, and what it leaves

**A commit is built once.** The git observer reports every ref each time it looks, and again from nothing when it
restarts. The orchestrator ignores a report in which the ref did not move (`old == new`), and does not build a commit it
has already taken for a build; a build that did not succeed is forgotten, so the commit is tried again when it is next
reported. A request on `polar.orchestrator.builds.requests` is always built (`src/observed.rs`).

**At most `max_concurrent_builds` run at once**; the rest wait (`src/admission.rs`).

**Finished jobs are removed by a retention policy** (`backend.kubernetes.retention` in the configuration file):

| Setting | Default | |
|---|---|---|
| `succeeded_secs` | 3600 | how long a job that succeeded stays |
| `failed_secs` | 86400 | how long a job that failed stays, for someone to read what Kubernetes recorded |
| `sweep_secs` | 300 | how often the orchestrator looks (never under 10) |

The orchestrator lists its own Jobs (the label `app.kubernetes.io/managed-by=build-orchestrator`), and removes those
whose Complete or Failed condition is older than the retention for how they ended. It never touches a Job that has not
finished, removes a Job together with its pods and its per-build Secret, and names the Job's uid in the delete so that a
Job made again under the same name is not the one removed. Each Job also carries Kubernetes' own expiry
(`ttlSecondsAfterFinished`) set to the longer of the two, for when the orchestrator is not running. The Role needs
`list` and `delete` on `jobs`, which it already has. In the deployment library this is `jobRetention`.

A build's logs are uploaded to the object store, and its provenance is published, as it finishes: before either expiry.
The object store itself has no retention yet.
