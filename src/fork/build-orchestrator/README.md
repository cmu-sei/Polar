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
