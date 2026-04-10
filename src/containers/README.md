# src/containers/

Infrastructure support containers for the Polar deployment pipeline.

## Why does this directory exist?

Polar's application agents live in `src/agents/`. Those are the binaries and
container images that implement Polar's observability features — they belong
to Polar-proper.

This directory contains container images that support the *deployment* of
Polar and its dependencies. They are infrastructure concerns:

- They may wrap third-party software (like Neo4j) at init time
- They run as Kubernetes init containers, not as long-lived workloads
- They have no direct dependency on Polar's Rust codebase
- They may eventually live in their own repository

## Contents

| Container | Image | Purpose |
|-----------|-------|---------|
| `nu-init` | `polar-nu-init:latest` | Generic nushell init container runner |

## Adding a new container

1. Create `src/containers/<name>/`
2. Write `container.dhall` following the `nu-init` pattern
3. Run `just render` in that directory to produce `container.nix`
4. Write `package.nix`
5. Import it in `workspace.nix`
6. Expose it in `flake.nix` under `packages.<platform>`
7. Add build + load targets to the root `Justfile`
8. Write a `README.md`
