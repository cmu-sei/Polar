<!--
  ACE Tool

  Copyright 2026 Carnegie Mellon University.

  NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
  FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
  EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
  PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
  MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
  TRADEMARK, OR COPYRIGHT INFRINGEMENT.

  Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
  for full terms.

  [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
  distribution.  Please see Copyright notice for non-US Government use and distribution.

  This Software includes and/or makes use of Third-Party Software each subject to its own license.

  DM26-0670
-->

# Cert Issuer

Short-lived X.509 client certificates for Polar agents, minted at
pod startup against a configured OIDC issuer (Kubernetes projected
service account tokens in v1) and signed by an in-process CA backed
by [`rcgen`](https://crates.io/crates/rcgen).

See the [specification document](../../../docs/architecture/cert-issuer-architecture.md) for the full
design rationale, threat model, and v2 directions.

## Package Layout

```
.
├── server
│   ├── tests
│   ├── conf
│   │   ├── dev.dhall
│   │   └── schema.dhall
│   ├── issuer-config.json
│   └── src
│       ├── ca.rs
│       ├── config.rs
│       ├── csr.rs
│       ├── handler.rs
│       ├── lib.rs
│       ├── main.rs
│       ├── oidc.rs
│       ├── server.rs
│       └── telemetry.rs
├── common
│   ├── Cargo.toml
│   └── src
│       ├── identity.rs
│       └── lib.rs
├── client 
│   ├── Cargo.toml
│   ├── tests
│   ├─ src
│       ├── handshake.rs
│       ├── keypair.rs
│       ├── lib.rs
│       ├── main.rs
│       ├── output.rs
│       └── token.rs
└── README.md
```

## Building

The workspace builds with stable Rust 1.75+. There are no native
build dependencies beyond a working `cargo` and a C linker.

```sh
cargo build --bin cert-issuer --bin cert-client --release
```

Two binaries land in `target/release/`:

- `cert-issuer` — the service
- `cert-client` — the init container client

For development:

```sh
cargo build -p cert-issuer -p cert-client -p cert-issuer-common
cargo test -p cert-issuer -p cert-client -p cert-issuer-common
```

## CA Model

The cert issuer holds a CA root keypair in memory and signs CSRs
directly using `rcgen`. There is no external CA process. The cert
issuer's only runtime dependencies are the OIDC issuer (Kubernetes
API server) and the volume holding its CA materials.

The tradeoff is that compromise of the cert issuer is compromise
of the CA root. For an internal-only PKI signing certs that only
agents on the Polar cluster trust, this is acceptable — the blast
radius is bounded by what those certs can do (talk to Cassini and
the credential agent). If you ever need to issue certs that anchor
external trust, swap the `RcgenCaClient` for an out-of-process CA
backend; the `CaClient` trait was designed for exactly this.

## CA Materials

The cert issuer manages its own CA materials: on startup, it looks
for a CA cert and key at the configured paths and bootstraps fresh
materials if they don't exist.

The state machine is straightforward:

| Cert file | Key file | Behavior                         |
|-----------|----------|----------------------------------|
| missing   | missing  | Bootstrap a fresh CA, write both |
| present   | present  | Validate and load                |
| present   | missing  | **Error:** partial state         |
| missing   | present  | **Error:** partial state         |

Within "both present," additional checks run:

- The key file's permissions must be exactly `0600` (Unix). Looser
  permissions are rejected — a key the host could share with other
  processes isn't owned by the cert issuer.
- The cert and key must form a matching pair. SubjectPublicKeyInfo
  bytes from the cert are compared to the key's public half;
  mismatches indicate someone replaced one but not the other.
- Both files must parse cleanly. Corrupt files surface as errors.

This is a deliberately strict model. Silent regeneration on any
inconsistency would either mask security problems (someone else
writing into the materials directory) or destroy CA materials that
downstream clients are still trusting. The only state we recover
from automatically is "neither file exists."

### What this means operationally

For first-run on a fresh volume: the cert issuer generates
materials, writes them with mode `0600` on the key, and proceeds.
The CA cert is logged at INFO; the CA key never leaves the process.

For steady state: the materials persist on the configured volume
and the cert issuer loads them on each restart. The CA root has a
10-year validity by default, and rotation is operator-driven.

For volume corruption or wrong mount: the cert issuer fails to
start with a clear error explaining which file is missing or
unreadable. This is intentional — silently regenerating would
issue certs from a new root that downstream clients don't trust,
producing TLS handshake failures across the system with no obvious
cause.

### Volume requirements in production

The CA materials directory must be **persistent across pod
restarts**. A Kubernetes Deployment with an `emptyDir` volume
would generate a fresh CA every restart and silently invalidate
all outstanding certs. Use a `PersistentVolumeClaim`, a mounted
Secret with `defaultMode: 0600`, or a similar persistent store.

To rotate the CA:

1. Distribute the new CA cert to all clients alongside the old
   one so clients accept either during the transition window.
2. Wipe the cert issuer's volume.
3. Restart the cert issuer (which bootstraps a fresh CA).
4. Once all certs issued under the old root have expired
   (`default_lifetime` after step 3), retire the old cert from
   clients.

### Distributing the CA cert to clients

The CA cert is public — clients verifying issued certs need it as
a trust root. After first run, copy it out of the volume:

```sh
# Local development
cat ./dev/tmp/ca.crt

# Kubernetes
kubectl exec -n polar deploy/cert-issuer -- cat /path/to/ca.crt
```

Distribute it as a ConfigMap mounted into client pods, or via
whatever trust-distribution mechanism the rest of your cluster
uses.

## Identity Normalization

Kubernetes service account tokens carry a `sub` claim in the form:

```
system:serviceaccount:<namespace>:<name>
```

This is not a valid DNS name. The cert issuer and init container
both normalize this to a DNS SAN using the shared function in
`cert_issuer_common::identity`:

```
system:serviceaccount:polar:git-observer
  -> git-observer.polar.serviceaccount.cluster.local
```

The normalization lives in `common/` so both sides are guaranteed
to use the same implementation. Drift between the two sides would
cause every issuance to fail with `IDENTITY_MISMATCH`.

## Configuration

cert-issuer is a Polar agent on the agent builder (`docs/architecture/cert-issuer-isolation.md`, "Step 1"). Its
settings are declared variables (`server/src/settings.rs`), read from its environment: one per scalar of the service
(`CERT_ISSUER_BIND_ADDR`, `CERT_ISSUER_CA_CERT_PATH`, the lifetimes, `CERT_ISSUER_SERVER_IDENTITY`,
`CERT_ISSUER_INTERNAL_CALLERS`, ...), and one JSON value each for the issuers (`CERT_ISSUER_ISSUERS`), the floor
(`CERT_ISSUER_BOOTSTRAP`) and the lifetime overrides (`CERT_ISSUER_IDENTITY_LIFETIME_OVERRIDES`). The whole is
checked before anything starts, by every rule `ServiceConfig::validate` applies. The builder's own flags list and check
them:

```sh
cargo run --bin cert-issuer -- --help       # every variable, with what it is for
cargo run --bin cert-issuer -- --scaffold   # the same, as the configuration document the render fills
```

In a cluster the chart writes them into a ConfigMap (`infra/layers/2-services/cert-issuer/configmap.dhall`). It also
needs its own service account token (`CERT_ISSUER_OWN_TOKEN`), on which it signs itself its client certificate through
its own floor, and the Cassini client's settings (`BROKER_ADDR`, `TLS_CLIENT_CERT`, ...): it speaks over Cassini once
Cassini is up, and does not depend on it. `conf/` holds the JSON file format the binary read before it moved onto the
builder; it is no longer read.

## Local Development

The dev setup is automated. One command generates all required
fixtures (CA keypair, OIDC signing keypair, JWKS document, test
token, dev config) and prints the exact commands to run:

```sh
cargo run --bin cert-issuer-setup
```

See it's output and follow the directions listed there.


The generated files under `dev/` are not secrets — the keypairs
are test-only and the token has no real cluster privileges. However, they should not be committed to version control.

To reset and regenerate everything:

```sh
# delete the files genreated by the setup bin
rm -rf dev 
# delete files generated by the cert-issuer server (provided tmp is where you stashed them, see the dhall config)
rm -rf tmp
cargo run --bin cert-issuer-setup
```

The token expires after one hour. Re-run `cert-issuer-setup` to
mint a fresh one; existing CA materials are preserved.

## Deploying

TODO: Write a section on deploying the cert-issuer in production


### Init container exit codes

The init container's exit code is what operators see in
`kubectl describe pod` when issuance fails. The mapping is:

| Code | Meaning                                                   | Action                        |
|------|-----------------------------------------------------------|-------------------------------|
| `0`  | Success — cert bundle written to `POLAR_CERT_DIR`         | —                             |
| `1`  | Misconfiguration — wrong audience, identity mismatch, bad token | Fix the pod spec, redeploy |
| `2`  | Transient failure — cert issuer unreachable, CA unavailable | Kubernetes will retry        |
| `3`  | Internal error — unexpected failure, malformed response   | Page someone                  |

The single most common misconfiguration is a missing or wrong
`audience` on the projected SA token volume — the cert issuer
rejects it with `INVALID_AUDIENCE` and the init container exits 1.
Check the projected token's `audiences` field first.

## Test Layout

### `agent/tests/`

| File                        | What it pins down                                                               |
|-----------------------------|---------------------------------------------------------------------------------|
| `config_test.rs`            | Config validation rejects HS256 and "none" at load time                         |
| `oidc_test.rs`              | Signature, audience, issuer, expiry, algorithm restrictions, JWKS caching       |
| `oidc_algorithms_test.rs`   | Every allowed algorithm, refusals, key/alg mismatch, nbf, iat, maximum lifetime |
| `csr_test.rs`               | CSR parsing, single-SAN enforcement, exact-string identity matching             |
| `handler_unit_test.rs`      | Outcome-to-status-code mapping and wire serialization stability                 |
| `handler_integration_test.rs` | End-to-end ordering: CA never called when upstream validation fails           |

CA tests (bootstrap state machine, key permission checks, cert-key
matching) live in `agent/src/ca.rs` as `#[cfg(test)] mod tests`.

### `init/tests/`

| File                 | What it pins down                                                                     |
|----------------------|---------------------------------------------------------------------------------------|
| `handshake_test.rs`  | Bearer token sent correctly, all response categories mapped to correct error variants |
| `keypair_test.rs`    | Distinct keypair per call, CSR parseable by cert issuer, Ed25519 algorithm enforced   |
| `output_test.rs`     | Three files written, key is mode 0640, atomic writes, existing files overwritten      |

### `common/tests/`

| File                 | What it pins down                                                                     |
|----------------------|---------------------------------------------------------------------------------------|
| `identity_test.rs`   | Kubernetes SA sub normalized to DNS form; non-Kubernetes sub rejected in v1           |

## What's Not Tested at This Layer

Deliberately deferred to end-to-end testing in a real cluster:

- TLS termination on the cert issuer's HTTPS endpoint (v1 serves plain HTTP)
- Cassini event publication (Phase B)
- Prometheus metrics emission
- Init container exit codes under subprocess invocation in CI

## v2 Directions

See Section 12 of the architecture spec for the full v2 backlog
with explicit trigger conditions.

- **Cosign signature verification** of the requesting workload's image
- **Workload-image attestation** via downward API
- **Multi-issuer OIDC support** for hybrid environments (GitLab CI,
  AWS IRSA, GCP Workload Identity) — see open issue on GitLab trust
  model before adding GitLab as an issuer
- **Bare-metal targets** via TPM or SPIRE node attestation
- **CA cross-signing** for zero-downtime rotation
