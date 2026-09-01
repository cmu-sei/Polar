# Kubernetes Observer and Consumer Agents

This repository contains microservices that observe Kubernetes clusters and process cluster data into a graph representation. The system consists of:

* **Observer Agents**: Collect resource data from a Kubernetes cluster and watch for events, stamping every observation with that cluster's identity.
* **Consumer Agents**: Transform and enrich this data, then publish it to a graph database, using that same identity to keep data from different clusters unambiguous.

## Multi-Cluster Identity

A single Cassini broker and a single consumer can serve multiple clusters at once. Every observer resolves the UID of its cluster's `kube-system` namespace once at startup — a permanent fixture in every cluster, requiring no manual configuration, and collision-safe across clusters by construction rather than by convention, since it's a real UUID assigned by the Kubernetes API server itself.

That `cluster_uid` is stamped onto every event an observer emits and carried through to every node and edge a consumer writes. Observers publish to a per-cluster topic (`polar.kubernetes.<cluster_uid>.events`) rather than a shared one, and consumers discover which clusters exist dynamically — via a broker-side announce/query handshake, not static configuration — subscribing to each cluster's topic as it's discovered. A consumer that restarts and missed earlier announcements re-triggers them by publishing to a query topic every currently-running observer is listening on.

## Getting Started

### Prerequisites

* Rust (latest stable, install via [`rustup`](https://rustup.rs/))
* Access to a Kubernetes cluster
* A valid `KUBECONFIG` file or in-cluster access
* mTLS certificates for secure service communication
* Running instance of the **Cassini** message broker - see [cassini's README for details](../cassini/broker/readme.md)

# Generating Flux CRD Types with kopium

The Rust types for Flux CRDs (`OciRepository`, `Kustomization`, etc.) in
`kube-common::flux` are generated from Flux's published CRD schemas using
[kopium](https://github.com/kube-rs/kopium). They are checked in as source
and should be regenerated whenever the Flux version used in the cluster is
upgraded.

## Prerequisites

Install kopium via cargo:

```sh
cargo install kopium
```

You will also need `yq` (the Go implementation — `mikefarah/yq`) for splitting
multi-document YAML files from the source-controller release. Verify you have
the right one:

```sh
yq --version
# should report: yq (https://github.com/mikefarah/yq/) version vX.X.X
```

On macOS: `brew install yq`. On Linux, grab the binary from the
[releases page](https://github.com/mikefarah/yq/releases) 

**NOTE: the Python wrapper distributed by some package managers is a different tool and its `-s` flag behaves differently.**

## Kustomize Controller CRDs

The kustomize-controller ships its CRDs as a single-document YAML file, so
kopium can consume it directly from the release URL.

Locate the latest release tag at
`https://github.com/fluxcd/kustomize-controller/releases` and substitute the
version below:

```sh
curl -sSL https://github.com/fluxcd/kustomize-controller/releases/download/v1.8.5/kustomize-controller.crds.yaml \
    | kopium -Af - > kube-common/src/flux/kustomization.rs
```

## Source Controller CRDs

The source-controller release bundles all of its CRDs (`OCIRepository`,
`GitRepository`, `HelmRepository`, `HelmChart`, `Bucket`,
`ExternalArtifact`) into a single multi-document YAML file separated by `---`.
kopium does not support multi-document YAML, so the file must be split first.

Download the release and split it by CRD name:

```sh
curl -sSL https://github.com/fluxcd/source-controller/releases/download/v1.5.0/source-controller.crds.yaml \
    | save /tmp/source-controller.crds.yaml

# Split into one file per CRD, named after metadata.name
open /tmp/source-controller.crds.yaml --raw
| split row "---\n"
| filter { |doc| ($doc | str trim) != "" }
| each { |doc|
    let name = ($doc | from yaml | get metadata.name)
    $doc | save $"/tmp/($name).yml"
}
```

This produces files like `ocirepositories.source.toolkit.fluxcd.io.yml`,
`gitrepositories.source.toolkit.fluxcd.io.yml`, etc. in `/tmp`.

Run kopium against the ones you need. Currently only `OCIRepository` is in
scope:

```sh
kopium -Af /tmp/ocirepositories.source.toolkit.fluxcd.io.yml \
    > kube-common/src/flux/oci_repository.rs
```

## Verifying the Output

After regenerating, confirm the status structs are fully typed — kopium will
silently fall back to `BTreeMap<String, serde_json::Value>` or omit `status`
entirely if the CRD schema does not fully specify the status subresource. The
fields you must verify are present and typed for each resource:

**`OciRepositoryStatus`** — must have `artifact: Option<OciRepositoryStatusArtifact>`
with `digest: String` and `revision: String` as non-optional fields.

**`KustomizationStatus`** — must have `last_applied_revision: Option<String>`,
`last_applied_origin_revision: Option<String>`, and
`conditions: Option<Vec<Condition>>`.

If either degrades to untyped fields, the CRD schema has changed in a
breaking way and the `GraphOperable` implementations in `kube-processor` will
need to be revisited before the generated files are committed.

## Keeping Types in Sync with the Cluster

The generated files represent a contract against a specific Flux API version.
The `#[kube(...)]` attributes at the top of each generated root type record the
group, version, and kind:

```rust
#[kube(group = "source.toolkit.fluxcd.io", version = "v1", kind = "OCIRepository", ...)]
```

If the cluster is upgraded to a Flux version that promotes a CRD to a new API
version (e.g. `v1beta2` → `v1`), the generated types must be regenerated and
the `kube-common` crate version bumped. Running the observer against a cluster
whose CRD version does not match the generated type will produce deserialization
errors at runtime, not compile time.

## Kubernetes Access

The observer agents use the [`kube`](https://docs.rs/kube) crate to authenticate and interact with the Kubernetes API. It will automatically detect configuration in the following order:

* The `KUBECONFIG` environment variable (if set)
* `$HOME/.kube/config`
* In-cluster configuration (if deployed as a pod)

If you're running locally, export your `KUBECONFIG`:

```bash
export KUBECONFIG=$HOME/.kube/config
```

---

### Run the Observer Agent

Each observer monitors a specific cluster and publishes messages to Cassini.

```bash
cargo run -b kube-observer
```

---

### Run the Consumer Agent

Consumers subscribe to messages from Cassini and process them into graph nodes/edges.

```bash
cargo run -b kube-consumer
```
---

## Testing

### Unit Tests

`cargo test` runs the unit tests embedded throughout each crate — `kube-common`'s fixture-parsing tests, the observer's mocked-apiserver tests (via [`tower-test`](https://docs.rs/tower-test), exercising the real LIST/WATCH/emit logic without a live cluster), and each agent's own logic tests. No external services required for any of these.

### Integration Tests: the Scenario Harness

`kube_common::testing` provides a reusable harness for exercising an agent's real graph-projection logic — `GraphOperable::project_into_graph`, in the kube-consumer's case — against a real Neo4j instance, without needing a live Kubernetes cluster or a running Cassini broker connection. A `Scenario` loads synthetic resources from YAML fixtures (ordinary Kubernetes manifests — the same shape as `kubectl get -o yaml`, not a bespoke test format), projects them through the actual production code path, and asserts against the resulting graph state directly.

The first such test, `consume/tests/pod_running_on_scenario.rs`, confirms an end-to-end chain: a `Node` fixture is observed, a `Pod` fixture referencing that node is observed, and the resulting graph actually contains a `RUNNING_ON` edge between them — resolved through the same name-to-UID cache the production consumer uses, since Kubernetes never exposes a Node's UID anywhere on a Pod, only its name.

This harness, and the broader goal of validating cross-agent graph projection without depending on the full production stack (Cassini, actor supervision, a live cluster) for every test, is tracked as an ongoing effort — see the project's issue tracker for the current scope and roadmap.

#### Prerequisites

Integration tests require a real Neo4j instance, provided via [`testcontainers-rs`](https://docs.rs/testcontainers). Before running:

* `GRAPH_ENDPOINT`, `GRAPH_USER`, `GRAPH_PASSWORD`, and `GRAPH_DB=neo4j` must already be set in the environment. The harness reads these first and configures the test container to match them, rather than starting a container and reporting its values back out — so `GRAPH_ENDPOINT` must specify a *fixed* port (e.g. `bolt://localhost:7687`), not one left for Docker to assign dynamically. `GRAPH_DB` must be `neo4j`: Neo4j Community Edition supports exactly one standard database, and it's always named that.
* A working Docker-compatible container runtime.

  **If you're on Podman** (e.g. NixOS with `virtualisation.podman.enable`), `dockerCompat` alone isn't sufficient — either enable the Docker-compatible socket in your NixOS configuration:

  ```nix
  virtualisation.podman.dockerSocket.enable = true;
  ```

  (requires being in the `podman` group — the same caveat as the Docker group: members can effectively gain root), or point `testcontainers` at Podman's own socket directly, without a system rebuild:

  ```bash
  systemctl --user enable --now podman.socket
  export DOCKER_HOST="unix://$XDG_RUNTIME_DIR/podman/podman.sock"
  export TESTCONTAINERS_RYUK_DISABLED=true
  ```

  Disabling Ryuk (testcontainers' cleanup sidecar, which rootless Podman generally can't grant sufficient privileges to) means a crashed test run won't clean up its own container automatically — check `podman ps` / `docker ps` occasionally if developing this way.

Run the integration tests:

```bash
cargo test -p kube-consume --test pod_running_on_scenario
```

(`kube-consume` above is an assumed package name, matching the same placeholder flagged directly in `pod_running_on_scenario.rs` — substitute whatever the `consume` crate's `Cargo.toml` actually declares if this doesn't resolve.)

Pass `--nocapture` to see tracing output from both the test itself and the production code paths it exercises — without it, `cargo test` only shows output for failing tests:

```bash
cargo test -p kube-consume --test pod_running_on_scenario -- --nocapture
```

#### Isolation Model

Neo4j Community Edition's one-database-only limitation means scenario tests share a single Neo4j instance and a single `GraphControllerActor` for the whole test binary, rather than one per test. Isolation instead comes from `cluster_uid`: every scenario generates its own, and — per the Multi-Cluster Identity section above — every node and edge this system writes is already scoped by it, so concurrent scenarios sharing one database can't see or collide with each other's data without any additional setup.
