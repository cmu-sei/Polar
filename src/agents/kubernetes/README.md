# Kubernetes Observer and Consumer Agents

This repository contains microservices that observe Kubernetes clusters and process cluster data into a graph representation. The system consists of:

* **Observer Agents**: Collect resource data from a Kubernetes cluster and watches for events.
* **Consumer Agents**: Transform and enrich this data, then publish it to a graph database.

## Getting Started

### Prerequisites

* Rust (latest stable, install via [`rustup`](https://rustup.rs/))
* Access to a Kubernetes cluster
* A valid `KUBECONFIG` file or in-cluster access
* mTLS certificates for secure service communication
* Running instance of the **Cassini** message broker - see [cassini's README for details](../cassini/broker/readme.md)

---
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
cargo run -p kube-consumer
```
