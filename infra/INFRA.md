# Polar Infrastructure Rendering Pipeline

This document describes the design, organization, and usage of the infrastructure
rendering pipeline living in `infra/`. If you are new to this system, read this
first before touching anything else.

---

## Why This Exists

Polar requires infrastructure across multiple deployment targets — a local
nix-usernetes cluster for development, a sandbox AKS environment, and an
emerging `ace` target, with more likely to follow. Each target shares a large
amount of structural intent but differs in image registries, TLS configuration,
storage classes, service topology, and platform-layer tooling (Flux, Istio, etc.).

The previous system (`src/deploy/`) had no clean abstraction for any of this.
Chart files reached directly for environment variables at Dhall evaluation time,
a `values-active.dhall` file was written at runtime by a bash script to simulate
target selection, `constants.dhall` contained fully-constructed Kubernetes
resources, and a single bash script (`render-manifests.sh`) knew nothing about
targets, ordering, or dependencies. The result was a system where secrets,
render logic, target selection, and Kubernetes resource definitions were all
entangled with one another.

This pipeline exists to fix that — cleanly, and in a way that scales to many
targets and to infrastructure concerns well beyond Polar's own pods.

---

## Design Principles

**1. Dhall is purely a data transformation layer.**
Dhall files take a well-typed values record as input and produce Kubernetes
resources as output. They do not reach for environment variables. They do not
make target-specific decisions. They do not know about other charts. A Dhall
chart file is a pure function: given values, produce resources.

**2. Nushell owns the pipeline.**
Target selection, environment validation, secret threading, render ordering, and
apply ordering are all nushell concerns. Nushell is data-native and sits
naturally between Dhall objects — it can consume `dhall-to-yaml` output as
structured data, transform it, and pass results forward. The pipeline has
explicit seams at every level.

**3. Every chart is a self-contained module.**
Each chart directory contains its own Dhall files (one per functional concern)
and a `main.nu` that is the local entrypoint for that chart. `main.nu` knows
which Dhall files belong to the chart, in what order they render, and how to
invoke `dhall-to-yaml` for each. The top-level `render.nu` does not know the
internals of any chart — it delegates downward.

**4. Layers express real dependencies.**
Infrastructure has a natural dependency order: platform primitives must exist
before services can run, services must exist before workloads can connect to
them, workloads must exist before there is anything to observe. The directory
structure makes this dependency order explicit and self-documenting. A new
engineer reading the tree understands the deployment order before reading a
single line of code.

**5. Secrets are a first-class, separately managed concern.**
Secrets are cross-cutting, environment-specific, and require tighter control
than chart resources. They live in their own top-level directory, mirroring the
layer structure of `layers/` so they are navigable by anyone who understands
the rest of the tree. Chart Dhall files never construct secrets from environment
variables — secrets are threaded into the pipeline by `render.nu` at render
time.

**6. Targets declare, they do not implement.**
A deployment target declares which charts it needs, in what order, with what
environment variables required, and what overrides to apply. It does not
re-implement chart logic. Target-specific behavior lives in `target.nu` (a
nushell script that can run setup logic and returns a resolved configuration
record) and `overrides.dhall` (a flat partial record that is merged against each
chart's canonical `values.dhall`).

---

## Directory Structure

```
infra/
  schema/               # Dhall type definitions and shared functions
  layers/               # All renderable infrastructure, organized by dependency
    1-platform/         # Must exist before everything else
      infra/            # Raw substrate: namespaces, storage
      services/         # Platform-layer services: cert-manager, flux, networking
    2-services/         # Shared stateful services: neo4j, cassini, jaeger
    3-workloads/        # Application workloads: Polar agents
      agents/
    4-observed/         # Staging/test infrastructure that Polar observes
  secrets/              # Secret definitions, mirroring the layer structure
    platform/
    services/
    workloads/
    shared/
  targets/              # One directory per deployment target
    local/
    sandbox/
    ace/
  render.nu             # Top-level pipeline orchestrator
  validate.nu           # Environment validation helpers
```

### Why Numbers on Layers?

The numeric prefixes on layer directories (`1-platform/`, `2-services/`, etc.)
are deliberate. They make the dependency order visible in every tool that shows
a directory listing — `ls`, `tree`, your editor's file browser, GitHub. No one
has to remember or look up which layer comes first. The filesystem tells you.

---

## The Layer Model

### Layer 1 — Platform

Everything that must exist before any service or workload can run. Divided into
two sub-concerns:

- **`infra/`** — the raw substrate: namespaces and storage classes. These have
  no dependencies and are applied first.
- **`services/`** — platform-layer services that everything above depends on:
  cert-manager (PKI), Flux (GitOps delivery), and networking (Istio
  VirtualServices). Not every target uses all of these — `local` has no Flux or
  Istio, for example. That is a target declaration, not a chart concern.

### Layer 2 — Services

Shared stateful infrastructure that application workloads depend on: Neo4j,
Cassini (the mTLS broker), and Jaeger (distributed tracing). These are not
Polar-specific — they could serve other consumers. Each chart in this layer
produces a set of Kubernetes resources and exposes outputs (DNS names, port
numbers, secret names) that layer 3 consumes.

### Layer 3 — Workloads

Polar's application layer: the agents that observe infrastructure and populate
the graph. Each agent group lives in its own subdirectory under `agents/` with
its own `main.nu` — a target can include any subset of agent groups it needs.

### Layer 4 — Observed

Staging and test infrastructure that Polar watches: GitLab instances, git
repositories, PKI for observed systems. This layer does not yet contain
anything, but its position in the dependency graph is correct — observed
infrastructure depends on the workloads that observe it being deployed first,
and it may itself have platform and service dependencies.

---

## How a Chart Works

Every chart directory follows the same pattern:

```
neo4j/
  statefulset.dhall     # StatefulSet definition
  service.dhall         # Service definition
  configmap.dhall       # ConfigMap
  pvcs.dhall            # PersistentVolumeClaims
  values.dhall          # Canonical default values for this chart
  main.nu               # Local entrypoint — owns rendering for this chart
```

**`values.dhall`** defines the canonical defaults for the chart as a typed
Dhall record. This is the chart's public interface — the shape of data it
expects.

**Individual Dhall files** are pure functions. They import from `schema/` for
types and shared functions, and they receive a values record as input. They
produce a list of Kubernetes resources as output. They do not import
`values.dhall` directly — the merged values record is passed to them by
`main.nu`.

**`main.nu`** is the chart's local entrypoint. It receives a context record
from `render.nu` (containing the merged values and any resolved outputs from
lower layers), invokes `dhall-to-yaml` for each Dhall file in the correct order,
and writes rendered YAML to the output directory. It is the only place that
knows the internal structure of the chart.

---

## How a Target Works

Each target directory contains:

```
targets/local/
  target.nu         # Setup logic + configuration declaration
  overrides.dhall   # Target-specific value overrides
  conf/             # Static config files (git.json, cyclops.yaml, etc.)
```

**`target.nu`** is an executable nushell script, not a pure data file. It can
run setup logic before render (loading certs from files, resolving paths,
calling external tools) and returns a single typed record:

```nushell
{
  charts: [                     # Ordered list of chart main.nu paths to invoke
    "layers/1-platform/infra/namespaces/main.nu"
    "layers/1-platform/services/cert-manager/main.nu"
    "layers/2-services/neo4j/main.nu"
    # ...
  ]
  env: {                        # Required environment variables, validated before render
    NEO4J_AUTH: "required"
    GRAPH_PASSWORD: "required"
    NEO4J_TLS_CA_CERT_CONTENT: "required"
    # ...
  }
  overrides: "targets/local/overrides.dhall"
  output_dir: "manifests"
  apply_order: [                # Ordered list of output files for kubectl apply
    "manifests/namespaces.yaml"
    "manifests/ca-issuer.yaml"
    # ...
  ]
}
```

The chart list and apply order are explicit per-target declarations. Different
targets can have completely different orderings. The ordering knowledge that was
previously hardcoded in a bash script now lives where it belongs — in the target
that needs it.

**`overrides.dhall`** is a flat partial record that is merged against each
chart's `values.dhall` at render time. It contains only what differs from
canonical defaults for this target: image tags, image pull secrets, storage
class names, registry URLs, and similar target-specific values.

---

## The Render Pipeline

```
target.nu
    │  returns: config record (chart list, env requirements, overrides path,
    │           output dir, apply order)
    ▼
render.nu
    │  validates env via validate.nu
    │  iterates chart list
    │  for each chart: merges overrides.dhall with chart values.dhall,
    │                  calls chart main.nu with resolved context
    ▼
chart main.nu  (repeated per chart)
    │  receives: merged values record + resolved layer outputs
    │  invokes: dhall-to-yaml for each Dhall file in correct order
    │  writes: YAML files to output directory
    ▼
output directory (e.g. manifests/)
    │  one YAML file per chart resource group
    ▼
kubectl apply  (ordered per target.nu apply_order)
```

The pipeline has no global state and no file-writing hacks. Each stage has a
typed interface. `render.nu` is thin — it orchestrates without knowing chart
internals. Chart `main.nu` scripts are isolated — they know their own internals
without knowing the pipeline.

---

## The Schema Directory

`schema/` contains the Dhall vocabulary that everything else is written in:

- **`kubernetes.dhall`** — Dhall bindings for Kubernetes resource types
- **`functions.dhall`** — shared helper functions (`makeDeployment`,
  `makeOpaqueSecret`, proxy cert helpers, etc.)
- **`agents.dhall`** — agent type definitions (`PolarAgent`, `GraphProcessor`,
  `GitlabObserver`, etc.)
- **`cassini.dhall`** — Cassini type definition
- **`storage-class.dhall`** — StorageClass type
- **`constants.dhall`** — truly static values only: namespace names, service
  names, TLS mount paths, port numbers, label and annotation constants. No
  environment variables. No constructed resources. No target-specific values.

If you find yourself wanting to put an environment variable or a constructed
Kubernetes resource into `constants.dhall`, it belongs somewhere else — in a
chart's `values.dhall`, in `secrets/`, or threaded through the pipeline by
`render.nu`.

---

## Secrets

Secrets live in `secrets/`, mirroring the layer structure:

```
secrets/
  platform/
    cert-manager/     # CA cert content, issuer credentials
    flux/             # Git repository credentials
  services/
    neo4j/            # NEO4J_AUTH, TLS cert/key content
    cassini/          # mTLS credentials
  workloads/
    agents/
      gitlab/         # GITLAB_TOKEN
      kube/           # SA token secrets
      git/            # git observer config
      build/          # build orchestrator SA token, cyclops config
  shared/             # Secrets consumed by multiple layers (GRAPH_PASSWORD,
                      # DOCKER_AUTH_JSON, image pull secrets)
```

Secrets are not rendered by the chart pipeline — they are managed separately
and applied before or alongside chart resources as the target's apply order
dictates. Chart Dhall files reference secrets by name (as `secretKeyRef` values)
but never construct them from environment variables.

---

## Adding a New Target

1. Create `targets/<name>/` with `target.nu`, `overrides.dhall`, and a `conf/`
   directory if needed.
2. In `target.nu`, declare the chart list (which `main.nu` scripts to invoke,
   in dependency order), required environment variables, path to
   `overrides.dhall`, output directory, and apply order.
3. In `overrides.dhall`, provide only the values that differ from each chart's
   canonical `values.dhall` defaults.
4. Run `render.nu <target>` to validate and render.

You do not need to modify any chart files, any schema files, or `render.nu`
itself to add a new target.

---

## Adding a New Chart

1. Decide which layer the chart belongs in based on its dependencies.
2. Create a directory under the appropriate layer.
3. Write individual Dhall files for each functional concern (deployment, service,
   configmap, etc.), each as a pure function that takes a values record.
4. Write `values.dhall` with canonical defaults.
5. Write `main.nu` as the local entrypoint: merge incoming overrides with
   `values.dhall`, invoke `dhall-to-yaml` for each file, write output.
6. Add the chart's `main.nu` path to whichever targets need it, in the correct
   position in their chart list and apply order.

You do not need to modify `render.nu` or any other chart to add a new chart.

---

## Known Issues and Migration Notes

- **SA token ordering** — on a fresh cluster reset, `kubectl apply` of
  `agents.yaml` may silently fail to create `build-processor-sa-token` and
  `kube-observer-sa-token` on the first pass because the ServiceAccounts must
  exist before the token controller can populate them. The apply step should
  retry the agents manifest after a short delay. This is tracked as an open
  issue and will be addressed in `target.nu` for the `local` target.

- **Migration from `src/deploy/`** — the old system is being migrated
  incrementally. During migration, the canonical rendered state of `src/deploy/local/`
  serves as a baseline for diffing against the new pipeline's output. Do not
  delete `src/deploy/` until the new pipeline produces identical output and has
  been validated against a live cluster cycle.
