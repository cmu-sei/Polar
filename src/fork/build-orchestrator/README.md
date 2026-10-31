# build-orchestrator on the fork

build-orchestrator is Polar's authoritative builder: it runs a repository's pipeline as a Kubernetes Job against pinned
source and reports what ran as provenance events (`../../agents/build-orchestrator/README.md`). This is that
orchestrator, ported to the ACE fork's libraries, as an agent built on `polar-agent-builder`.

It is a workspace of its own in the same branch because the fork's `polar` library is not the public one (both
are named `polar`), so the two cannot be built together. The fork is a sibling checkout: `~/projects/ACE` next
to `~/projects/Polar`.

| | Ported from `src/agents/build-orchestrator` |
|---|---|
| `core` | domain types and the backend trait, unchanged |
| `k8s` | the Kubernetes backend (jobs), unchanged for now |
| `agent` | the orchestrator: a `PolarAgent` (the builder owns the broker, certificates, health and shutdown); the repositories it builds are the signed runtime section `mappings` |

Not ported: the Podman backend, and dependency analysis (the analysis scheduler, and the events and graph
nodes it needs, which the fork's library does not have yet).

## Building

The builder depends on the Linux kernel keyring, so it builds on Linux only. On a Mac, use the Linux VM
(`orb -m nix-builder`) and the fork's development shell, which has the toolchain:

```sh
cd ~/projects/ACE && nix develop --command bash -c \
  'cd ~/projects/Polar/src/fork/build-orchestrator && CARGO_TARGET_DIR=~/build-orchestrator-target cargo test --workspace'
```

`CARGO_TARGET_DIR` keeps build output out of both trees. Inside the VM `git` and `cargo` may need
`GIT_SSL_CAINFO`, `SSL_CERT_FILE` and `CARGO_HTTP_CAINFO` set to `/etc/ssl/certs/ca-bundle.crt`.

`build-orchestrator --scaffold` prints the settings and the `mappings` section.

## Images

Two, both built in the VM with nix and loaded into the node:

- `image.nix`: build-orchestrator, from the `build-orchestrator` binary (cargo, above) and the fork's
  `polar-healthcheck`, which the liveness probe runs. This is the local route; a crane build over both
  source trees is the way to a reproducible one.
- `object-store-image.nix`: an S3-compatible store for the jobs' logs (the Versity gateway over a volume).
  On a cloud a bucket replaces it.

Both take the fork's pinned nixpkgs, e.g. `nix build --impure --expr 'let ace = builtins.getFlake
"git+file://$HOME/projects/ACE"; pkgs = ace.inputs.nixpkgs.legacyPackages.aarch64-linux; in import
./image.nix { inherit pkgs; orchestrator = <path>; healthcheck = <path>; }'`.

## Deploying

`types/build-orchestrator.dhall` in the deployment library is the stage: the agent, its Role in the jobs' namespace, the
store, and the secret that opens it. Blitz's plan includes it after the git agents.

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
