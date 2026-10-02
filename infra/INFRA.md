# Polar Infrastructure Pipeline

How Polar is deployed to Kubernetes, and how to add to it: an agent, a
capability, a service, an image, a target. If you are new to this system, read
this first before touching anything under `infra/`.

If you only want to bring a cluster up, read [Day to day](#day-to-day) and stop.
If you are adding something, read [How it fits together](#how-it-fits-together)
once, then go to the recipe you need.

---

## Why this exists

Polar needs infrastructure across several deployment targets: a local
nix-usernetes cluster for development, a sandbox AKS environment, an emerging
`ace` target, and more to come. Each shares a large amount of structural intent
but differs in image registries, TLS configuration, storage classes, service
topology and platform tooling (Flux, Istio, and so on).

The first system (`src/deploy/`) had no clean abstraction for any of this.
Chart files reached for environment variables at Dhall evaluation time, a
`values-active.dhall` was written at runtime by a bash script to simulate target
selection, `constants.dhall` held fully constructed Kubernetes resources, and a
single bash script knew nothing about targets, ordering or dependencies. Secrets,
render logic, target selection and resource definitions were all entangled.

The layered pipeline fixed that: Dhall as pure transformation, nushell owning the
pipeline, charts as self-contained modules, targets that declare rather than
implement.

It left a second problem. Every agent was still a hand-written chart repeating the
same Job skeleton, and the facts that distinguish one agent from another (its
image, its dependencies, its secrets, its RBAC) were scattered across `target.nu`,
`overrides.dhall`, `constants.dhall`, each chart's `main.nu`, `render.nu` and the
Rust code. Per-agent TLS configuration was already identified as the largest
single source of configuration drift (see
`docs/architecture/cert-issuer-architecture.md`). The **configuration catalog**
addresses that: an agent is a short entry of data, and what each dependency means
is stated once. The design record is
`docs/architecture/config-catalog-design.md`.

---

## The idea in one table

Everything under `infra/` belongs to one of three tiers. Each has one job, and a
change should land in exactly one of them.

| Tier | Job | Lives in | Rule |
|---|---|---|---|
| **Configuration** | The facts: what exists and what it needs | nuon files (`infra/config/**`, `infra/images.nuon`, `infra/targets/<t>/config.nuon`) | Data only. No logic. |
| **Structure** | The shape of a Kubernetes object | Dhall (`infra/schema/**`, the custom charts under `infra/layers/**`) | A pure function of a typed record. No facts of its own. |
| **Automation** | The verbs: resolve, render, apply, wait | nushell (`up.nu`, `render.nu`, `resolve.nu`, `render-catalog.nu`, `images.nu`) | Orchestration. As thin as it can be. |

Between configuration and structure sits the **resolver** (`resolve.nu`). It reads
the data, applies layering and capabilities, and produces one fully resolved
record per workload. It is a pure function: no cluster, no network, no files
written. That is what makes it testable and what makes `explain` possible.

**Variation between agents is data.** An agent that needs Neo4j says
`needs: ["neo4j"]`. It does not get a `hasGraph` flag in a template. If you want a
boolean in a template to make one workload fit, stop: either the variation belongs
in a capability (data), or the workload is a custom chart (see
[the escape hatch](#add-a-custom-chart-the-escape-hatch)).

---

## Design principles

**1. Dhall is purely a data transformation layer.** Dhall files take a well-typed
record as input and produce Kubernetes resources. They do not reach for
environment variables, make target-specific decisions, or know about other
charts. A Dhall file is a pure function: given values, produce resources. Keep it
simple: plain records and lists, `Optional` instead of unions, no Text comparison.
Complex Dhall is its own hell, so push logic into the resolver, where it can be
tested.

**2. Nushell owns the pipeline.** Target selection, environment validation, secret
threading, render ordering and apply ordering are nushell concerns. Nushell is
data-native and sits naturally between Dhall objects. The pipeline has explicit
seams at every level.

**3. Configuration is data, and it is stated once.** A fact (an address, an image
tag, a dependency) has one home. Other places derive it.

**4. Custom charts are self-contained modules.** A chart directory holds its Dhall
files (one per functional concern) and a `main.nu` that is its local entrypoint.
`render.nu` does not know the internals of any chart.

**5. Layers express real dependencies.** Platform primitives must exist before
services, services before workloads, workloads before anything there is to
observe. The directory structure makes that order visible.

**6. Targets declare, they do not implement.** A target declares which charts it
needs, in what order, and what to override. It does not re-implement chart logic.

**7. Rendered manifests are the contract.** A refactor is correct when the render
before and after is the same; a feature is correct when only the intended
differences appear. Compare, do not eyeball.

---

## Day to day

Run everything from the repo root with `KUBECONFIG` pointing at the cluster.

```fish
# Bring a cluster up from a target (resumable: re-run the same command after a failure or pause)
nu infra/up.nu local-minimal --dev          # --dev builds and pushes the images the target needs

# Only render (never touches a cluster); output goes to manifests/ (gitignored)
nu infra/render.nu local-minimal

# What does the catalog say about a workload, and where did each value come from?
nu infra/resolve.nu explain kube-consumer

# Is the whole catalog consistent? (no cluster, no render)
nu infra/resolve.nu check

# Images
nu infra/images.nu list                     # every image the repo knows how to produce
just image kube-observer                    # build and push one
just images-all                             # build and push everything in the table
```

`up.nu` prints its plan first and skips what the target does not need. Its steps:
preflight, render, images (`--dev`), namespaces, cluster CA, secrets and
infrastructure, waiting for core services, port-forwards, admin flow, browser
certificate and Neo4j password. It stops for a human only where a human is
required (closing the browser, the Neo4j password).

---

## Directory structure

```
infra/
  up.nu                  brings a cluster up from a target (applies)
  render.nu              renders a target (never applies)
  resolve.nu             the resolver: explain, json, resources, diff, check
  render-catalog.nu      renders every catalog workload
  compare-manifests.nu   compares two renders by meaning
  images.nu  images.nuon which images exist, and building/pushing them
  validate.nu            environment validation helpers
  config/                the catalog
    cluster.nuon         domain, namespaces, registry, token audience
    services.nuon        in-cluster services other things connect to
    kinds.nuon           defaults shared by every workload of a kind
    capabilities.nuon    what each `needs` entry contributes
    facets.nuon          the operations every capability must answer
    workloads/*.nuon     one entry per catalog workload
  schema/                Dhall vocabulary
    agent.dhall  agent-spec.dhall          the Job template and its input type
    resources.dhall  resources-spec.dhall  RBAC and token Secrets alongside a workload
    functions.dhall  constants.dhall ...   shared helpers and static constants
  layers/                custom charts, by dependency
    1-platform/          must exist before everything else
      infra/             namespaces, storage
      services/          cert-manager, flux, networking (targets opt in)
    2-services/          shared services: cert-issuer, cassini, neo4j, jaeger, ...
    3-workloads/agents/  agents not yet in the catalog
    4-observed/          infrastructure Polar observes (empty so far)
  targets/<t>/
    target.nu            charts to render and the apply order
    overrides.dhall      values for the custom charts
    config.nuon          overrides for the catalog (cluster facts, per-workload fields)
    secrets.sops.yaml  secrets.nu   encrypted secrets and the script that creates them
    conf/                static config files (neo4j conf, git.json, ...)
```

The numeric prefixes on the layers are deliberate: the dependency order shows in
every directory listing.

### Layer model

- **1-platform.** Everything that must exist before any service or workload.
  `infra/` is the raw substrate (namespaces, storage classes) applied first;
  `services/` holds platform services (cert-manager, Flux, Istio networking). Not
  every target uses all of them: `local` has no Flux or Istio. That is a target
  declaration, not a chart concern.
- **2-services.** Shared stateful infrastructure workloads depend on: Neo4j,
  Cassini (the mTLS broker), Jaeger, cert-issuer, Keycloak, Postgres. They are not
  Polar-specific.
- **3-workloads.** The agents that observe infrastructure and populate the graph.
  Agents that are in the catalog have no directory here; they are entries in
  `config/workloads/`.
- **4-observed.** Staging and test infrastructure Polar watches. Empty so far, but
  its place in the dependency order is right.

---

## How it fits together

```
infra/config/*.nuon             cluster, services, kinds, capabilities, facets
infra/config/workloads/*.nuon   one entry per catalog workload
infra/images.nuon               which images exist and where they come from
infra/targets/<t>/config.nuon   per-target overrides
        │
        ▼   resolve.nu          pure; layering, capabilities, substitution
a resolved record (JSON)
        │
        ▼   json-to-dhall       typechecks it against schema/agent-spec.dhall
        ▼   schema/agent.dhall  the one Job template for agents
        ▼   dhall-to-yaml
manifests/<name>.yaml           (+ manifests/<name>-resources.yaml if it has RBAC etc.)
```

`render.nu` does this for every catalog workload (through `render-catalog.nu`),
after rendering the custom charts. Then it runs checks over the whole output: each
workload has a unique ServiceAccount (it becomes the SAN in the workload's
certificate), cert-issuer's TLS identity matches the URL callers use, and the CA
provisioning mode is consistent. It emits a ServiceAccount for every workload
identity no manifest declares. `up.nu` then applies the files in the target's
`apply_order`.

Custom charts follow the original pipeline:

```
target.nu  ->  render.nu  ->  chart main.nu (one per chart)  ->  manifests/*.yaml
  config      validates env,   dhall-to-yaml per Dhall file,
  record      builds context   writes YAML
```

### The vocabulary

A workload declares `needs`, a list of **capabilities**. Each capability answers
every **facet**:

| Facet | What it answers |
|---|---|
| `pod` | env, volumes and mounts the agent container gets |
| `identity` | client certificates polar-init obtains from cert-issuer (each also adds its volume, mounts, and a health-checked cert path) |
| `dependency` | the service whose certificate must be valid before the agent starts, and for how long |
| `resources` | Kubernetes objects that live alongside the workload (cluster roles, token Secrets) |

Today's capabilities are `cassini`, `neo4j` and `kube-api-read`. Every `agent`
gets `cassini` from its kind. From a workload's `needs` the resolver derives,
without anyone writing them per agent:

- the init container's `--check-dep-cert host:port:ttl` arguments (don't start an agent whose dependency's certificate is about to expire)
- `POLAR_HEALTH_DEP_ENDPOINTS` and `POLAR_HEALTH_CERTS` for the health actor
- the volumes, mounts and `--cert` specs for every client certificate

### Layering

For scalar and record fields, later wins: **kind defaults** (`kinds.nuon`) <
**workload entry** (`workloads/*.nuon`) < **target override**
(`targets/<t>/config.nuon`, under `workloads.<name>`). Lists (`needs`, `env`) are
unioned in declared order instead of replaced. `explain` prints each value with
its origin: `[kind]`, `[entry]`, `[target]`, `[<capability>]` or `[derived]`.

---

## How a custom chart works

```
neo4j/
  statefulset.dhall     one Dhall file per functional concern
  service.dhall
  configmap.dhall
  pvcs.dhall
  values.dhall          canonical default values for this chart
  main.nu               local entrypoint: owns rendering for this chart
```

`values.dhall` is the chart's public interface: the typed defaults it expects. The
Dhall files are pure functions over a values record, importing types and helpers
from `schema/`. `main.nu` receives a context record from `render.nu` (`chart_dir`,
`output_dir`, `overrides`, and any extra facts `render.nu` passes for that chart),
merges the target's overrides over `values.dhall`, runs `dhall-to-yaml` on each
Dhall file in order, and writes YAML to the output directory. It is the only place
that knows the chart's internals.

## How a target works

```
targets/local-minimal/
  target.nu         setup logic + the configuration record
  overrides.dhall   per-deployment values for the custom charts
  config.nuon       overrides for the catalog
  secrets.sops.yaml encrypted secrets
  secrets.nu        creates the Secrets and ConfigMaps from them
  conf/             static config files
```

**`target.nu`** is an executable nushell script, not a data file. It can run setup
logic (resolve paths, read the environment) and prints one nuon record: `charts`
(the `main.nu` paths to render, in dependency order), `apply_order` (the output
files, in order), `output_dir`, `overrides`, `required_env`, plus the cluster
facts still passed to the custom charts (`neo4j_bolt_addr`, `cert_issuer_url`,
`polar_init_image`, `identity_lifetime_overrides`, and so on). Different targets
can have completely different orderings.

**`overrides.dhall`** is a flat record merged over each custom chart's
`values.dhall`. It holds only what differs from the defaults for this target:
image references, pull secrets, storage classes, hostnames, URLs.

**`config.nuon`** is the catalog's override layer for this target:

```nuon
{
  cluster: { registry: "registry.example.com" }     # any field of cluster.nuon
  workloads: {
    "jira-observer": { backoff_limit: 5 }           # any field of a workload entry
  }
}
```

---

## The schema directory

`schema/` is the Dhall vocabulary:

- **`agent.dhall`, `agent-spec.dhall`**: the agent Job template and the record type it takes.
- **`resources.dhall`, `resources-spec.dhall`**: the objects that live alongside a workload (ClusterRole, binding, token Secret).
- **`kubernetes.dhall`**: Dhall bindings for Kubernetes types.
- **`functions.dhall`**: shared helpers (`makeDeployment`, `makePolarInitContainer`, the proxy cert helpers, ...).
- **`agents.dhall`, `cassini.dhall`, `storage-class.dhall`, `identity-strategy.dhall`, `ca-provisioning.dhall`**: type definitions.
- **`constants.dhall`**: static values: namespace names, service names, TLS paths, ports, labels. Its header says it holds no constructed resources and no environment-dependent values, but it still contains a few (`certEmptyDirVolume`, `saTokenVolume`, `commonClientEnv`); they are on the list to move out.

---

## Secrets

Secrets are encrypted per target and are not rendered by the chart pipeline.

- `targets/<t>/secrets.sops.yaml` holds the values, encrypted with sops (age). Add your key to the target's `.sops.yaml` and run `sops updatekeys` to give someone access.
- `targets/<t>/secrets.nu` decrypts them and creates the Secrets and ConfigMaps in the cluster. It creates what is missing and skips what already exists, so re-running is safe. `up.nu` runs it in the "secrets and infrastructure" step.
- Manifests only *reference* secrets by name (`secretKeyRef`); they never contain secret values. A catalog workload's env entry does the same: `{ name: "JIRA_TOKEN", secret: "jira-secret", key: "token" }`.
- Never `git add` certificates or keys.

(The `infra/secrets/` directory tree that the first version of this pipeline
described is empty and unused; secrets are per target now.)

---

## Recipes

### Add an agent to the catalog

Prerequisite: the agent's image builds from the flake. If it is not on
`polar-agent-builder` yet, migrate it first; the manifest side assumes the
capability names will line up with the builder's.

1. **Add the image row** to `infra/images.nuon` (one row; nothing else needs to know):
   ```nuon
   {image: "foo-observer", tag: "latest", kind: "nix", attr: "polarPkgs.fooAgent.observerImage"}
   ```
2. **Add the workload** to a file under `infra/config/workloads/` (all files there are read):
   ```nuon
   {
     name: "foo-observer"
     kind: "agent"
     image: "foo-observer"          # a row of images.nuon; its tag is used
     needs: ["neo4j"]               # on top of what the kind gives: cassini
     env: [
       { name: "FOO_URL",   value: "https://foo.example.com" }
       { name: "FOO_TOKEN", secret: "foo-secret", key: "token" }
     ]
   }
   ```
   Any field `kinds.nuon` defines can be overridden on the entry (`backoff_limit`,
   `probe`, `namespace`, `service_account`, ...).
3. **Secrets**: a `secret:` reference must exist in the cluster; add it to the
   target's `secrets.sops.yaml` and `secrets.nu`.
4. **Target-specific values** (a URL that differs per environment) go in
   `targets/<t>/config.nuon` under `workloads.foo-observer`, not in the entry.
5. **Check and look**:
   ```fish
   nu infra/resolve.nu check
   nu infra/resolve.nu explain foo-observer
   ```
6. **Apply order**: add `foo-observer.yaml` to `apply_order` in the target's
   `target.nu`, and `foo-observer-resources.yaml` *before* it if the workload has
   resources. This list is still hand-maintained.
7. **Render, compare, bring up** (see [Verifying a change](#verifying-a-change)).

The ServiceAccount is automatic: the workload runs as `service_account` (default:
its own name), and `render.nu` emits the ServiceAccount object for it.

### Add a capability

A capability is something several workloads can need: a service connection, a
permission, a credential.

1. Add an entry to `infra/config/capabilities.nuon` that **answers every facet** in
   `facets.nuon`. Use `{}` where it has nothing to add. The file's header documents
   each facet's shape.
2. Text values can use `{<service>.dns}`, `{<service>.port}`, `{<service>.addr}`
   and `{workload}`.
3. If it connects to a service that is not in `services.nuon`, add the service first.
4. Use it: `needs: ["your-capability"]`.
5. `nu infra/resolve.nu check` fails if a facet is unanswered, an unknown facet is
   answered, a dependency names a service that does not exist, or a placeholder
   cannot be resolved. You cannot forget a case.

Name it for what the code *requires*: the Rust agent builder's capability methods
should use the same names (`.with_graph_write()`, `.with_encryption()`). That link
is planned but not enforced yet; see [Known limits](#known-limits).

### Add a service

One entry in `infra/config/services.nuon`:

```nuon
foo: { dns_name: "foo-svc", namespace: "polar", port: 9000 }
```

`namespace` is a key of `cluster.nuon`'s `namespaces`, not a literal name. The
address is `<dns_name>.<namespace>.svc.<cluster.domain>:<port>`, so changing the
cluster domain in one place changes every address. The entry does not deploy
anything; it tells other workloads where to find the service. Deploying it is a
custom chart until services are in the catalog.

### Add an image

One row in `infra/images.nuon`. Three kinds:

| kind | for | columns |
|---|---|---|
| `nix` | built from this repo's flake (`.#packages.<platform>.<attr>`) | `attr` |
| `mirror` | a third-party image copied into the local registry | `upstream` |
| `external` | built outside this repo, an existing docker-archive | `path` (`~` allowed), `env` (variable that overrides it) |

The rendered manifests decide which images a target needs; `images.nu` joins their
`image:` fields against this table. A manifest image with no row, or a tag that
disagrees with the row, fails before anything is built.

### Change a value for one target

Use `targets/<t>/config.nuon` (see [How a target works](#how-a-target-works)).
Scalars override; `needs` and `env` entries are appended.

### Add a custom chart (the escape hatch)

Use this when a workload does not fit a catalog kind: a StatefulSet, a Deployment
with a PVC, anything with more structure than an agent Job. This is what the
existing services are today.

1. Pick the layer by dependency and create `infra/layers/<layer>/<name>/` with a
   Dhall file per concern, a `values.dhall`, and a `main.nu` (merge the incoming
   overrides with `values.dhall`, run `dhall-to-yaml` per file, write the output).
2. Add `main.nu` to `charts` in each target that needs it, and its output files to
   `apply_order`.
3. If the chart needs cluster facts, they come through `render.nu`'s context. Today
   that is a `match` on the chart name; replacing it with data is on the list.
4. Per-deployment values go in `overrides.dhall`.

Keep a custom chart's Dhall simple. When two charts start repeating the same
skeleton, promote the skeleton to a kind.

### Add a kind

A kind is a template plus defaults for a family of workloads that share a shape
(today: `agent`).

1. Defaults in `kinds.nuon`.
2. A template `schema/<kind>.dhall` taking a typed record, and its type in
   `schema/<kind>-spec.dhall`. Use only Text, Natural, Bool, Optional and List
   (Dhall cannot compare Text, and the JSON encoding of unions is fragile); read
   "which variant is this" off which `Optional` field is set.
3. Register it in `TEMPLATES` in `render-catalog.nu`.
4. The resolver builds the agent-shaped record today; a kind with a different shape
   needs the resolver to produce its own record.

### Add a facet

Rare. A facet is a new operation every capability must answer.

1. List it in `facets.nuon` with a one-line description.
2. Give every existing capability an answer (`check` lists the missing ones).
3. Teach `resolve.nu` to collect it and a template to use it.

### Add a target

1. Copy `infra/targets/local-minimal/`.
2. `target.nu`: the charts it deploys, `apply_order`, and the values still passed
   through it.
3. `overrides.dhall`: values for the custom charts.
4. `config.nuon`: the `cluster` facts that differ (`registry`, `domain`) and
   per-workload overrides.
5. Secrets: `.sops.yaml` with your keys, `secrets.sops.yaml`, `secrets.nu`.
6. `nu infra/resolve.nu check --target <t>`, then `nu infra/render.nu <t>`.

You do not modify any chart, schema file or `render.nu` to add a target.

---

## Verifying a change

Rendered manifests are the contract. For a change that should not alter what is
deployed (a refactor, a migration), prove it. For one that should, check that only
the intended differences appear.

```fish
# 1. Keep a render from before your change (untracked; add manifests.old/ to .git/info/exclude once)
nu infra/render.nu local-minimal
rm -rf manifests.old; cp -r manifests manifests.old

# 2. Make your change, then render again
nu infra/render.nu local-minimal

# 3. Compare by meaning, not by text
nu infra/compare-manifests.nu manifests.old manifests kube      # names after the dirs filter by Kind/namespace/name
```

`compare-manifests.nu` keys every document by Kind, namespace and name, so a
document that moved between files still matches. Lists of named things (env,
volumes, mounts, containers) are compared in name order. It prints `same`, or one
line per difference, and exits non-zero if there are any.

For a quick check with no render, `nu infra/resolve.nu diff <workload> --manifests
manifests.old` compares what the resolver says with a rendered manifest, and
`nu infra/resolve.nu resources <workload>` prints the objects it will emit
alongside the workload.

Then bring it up on a cluster and look at the pods, not just the apply output.

---

## Reading errors

| You see | It means |
|---|---|
| `unknown need 'x'` | `x` is not in `capabilities.nuon` |
| `capability 'x' does not answer facet(s): ...` | a facet in `facets.nuon` is missing from the capability; add `{}` or the real answer |
| `capability 'x' answers unknown facet(s)` | a typo, or a facet not listed in `facets.nuon` |
| `dependency names service 'x', which is not in services.nuon` | add the service, or fix the name |
| `unresolved placeholder in '...'` | a `{svc.dns}` names a service `services.nuon` does not have |
| `image 'x' has no row in infra/images.nuon` | add the row |
| `identity a.b is shared by: ...` | two workloads share a ServiceAccount; give one its own `service_account` |
| `the resolved record does not match agent-spec.dhall` | resolver and template disagree about a field; change both together |
| `no template for kind 'x'` | register the kind in `TEMPLATES` in `render-catalog.nu` |
| `json-to-dhall` / `dhall-to-yaml` not on PATH | the `dhall-json` package is missing from the shell (it comes from nix) |

---

## Rules of the road

- **Put each change in the right tier.** A new fact is nuon. A new shape is Dhall. A new step is nushell.
- **Variation is data.** No per-agent booleans in templates.
- **Capabilities answer every facet.** `check` enforces it; keep it true.
- **Compare before and after.** Rendered output is the contract.
- **Keep Dhall simple.**
- **Never `git add` certificates or keys.**
- **Do not change product code to suit the dev environment.**

nushell gotchas that have bitten this pipeline:

- `env` is a reserved variable name.
- Closures cannot capture a `mut` variable; return a record or list and collect.
- Annotate mutable lists: `mut xs: list<any> = []`. Newer nushell infers the element type from the first value and rejects a later append of a different shape.
- Do not shadow builtins (`table`, `select`).
- `do -i ... | complete` does not catch a missing command; use `try`.
- `input` needs a TTY.

---

## Known issues and limits

- **Service-account token ordering.** On a fresh cluster, a `kubernetes.io/service-account-token` Secret is populated by the token controller only after its ServiceAccount exists, so the first apply may need repeating. `serviceaccounts.yaml` precedes `kube-observer-resources.yaml` in `apply_order`, which should avoid it, but this has only been exercised on an existing cluster.
- **Only `agent` is a catalog kind, and only the kube agents are in the catalog.** Everything else is a custom chart.
- **`apply_order` is hand-maintained** in each `target.nu`. It should be derived from the catalog (resources before the Job).
- **`render.nu` passes cluster facts to custom charts through a `match` on the chart name,** and `target.nu` carries values (`cert_issuer_url`, `polar_init_image`, ...) that the catalog derives elsewhere. That is data written as control flow, and a duplicated fact.
- **Capability names are meant to be shared with the Rust agent builder** (one declaration driving both the manifest and a startup check that the deployment provides what the code requires). Neither the rename nor the check exists yet.
- **The cluster domain drives catalog addresses,** but custom charts still hard-code `svc.cluster.local`, and `polar-init --cluster-domain` is not passed.
- **`src/deploy/`** is the old system. Confirm it is dead and remove it, or mark it.

Open items are tracked in `remaining-work.md`.
