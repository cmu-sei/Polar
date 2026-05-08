# Cert Issuer

Short-lived X.509 client certificates for Polar agents, minted at
pod startup against a configured OIDC issuer (Kubernetes projected
service account tokens in v1) and signed by an in-process CA backed
by [`rcgen`](https://crates.io/crates/rcgen).

See [`architecture-spec.md`](architecture-spec.md) for the full
design rationale, threat model, and v2 directions.

## Package Layout

```
.
├── agent
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
├── init
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
cargo build --bin cert-issuer --bin cert-issuer-init --release
```

Two binaries land in `target/release/`:

- `cert-issuer` — the service
- `cert-issuer-init` — the init container client

For development:

```sh
cargo build -p cert-issuer -p cert-issuer-init -p cert-issuer-common
cargo test -p cert-issuer -p cert-issuer-init -p cert-issuer-common
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
kubectl exec -n polar-system deploy/cert-issuer -- cat /etc/cert-issuer/ca.crt
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

Config is provided as a JSON file. The path is set via the
`CERT_ISSUER_CONFIG` environment variable. Dhall is the source of
truth for config authoring; JSON is what the binary reads.

```sh
dhall-to-json --file conf/dev.dhall > /tmp/cert-issuer.json
CERT_ISSUER_CONFIG=/tmp/cert-issuer.json cargo run --bin cert-issuer
```

A representative config (JSON):

```json
{
  "bind_addr": "127.0.0.1:8443",
  "ca": {
    "ca_cert_path": "./dev/tmp/ca.crt",
    "ca_key_path": "./dev/tmp/ca.key",
    "default_lifetime": { "secs": 1800, "nanos": 0 }
  },
  "issuer": {
    "issuer": "https://kubernetes.default.svc",
    "audience": "polar-cert-issuer.prod",
    "jwks_uri": "https://kubernetes.default.svc/openid/v1/jwks",
    "workload_identity_claim": "sub",
    "instance_binding_claim": "kubernetes.io/pod/uid",
    "allowed_algorithms": ["RS256", "ES256", "EdDSA"],
    "jwks_cache_ttl_min": { "secs": 30, "nanos": 0 },
    "jwks_cache_ttl_max": { "secs": 3600, "nanos": 0 }
  }
}
```

Two fields have no defaults and must be set per-environment:

- `issuer.issuer` — the OIDC issuer URL the cert issuer trusts
- `issuer.audience` — the audience tokens must claim

The Rust validator at startup rejects empty values for these fields.

### TLS

v1 does not terminate TLS. The binary serves plain HTTP on the
configured `bind_addr`. TLS is the responsibility of whatever
fronts it — a service mesh sidecar, ingress controller, or sidecar
proxy. In Kubernetes, a NetworkPolicy restricting access to the
cert issuer's pod is strongly recommended regardless.

## Local Development

The dev setup is automated. One command generates all required
fixtures (CA keypair, OIDC signing keypair, JWKS document, test
token, dev config) and prints the exact commands to run:

```sh
cargo run --bin cert-issuer-setup
```

Output:

```
--- Step 1: generating CA keypair and self-signed cert ---
  wrote dev/tmp/ca.crt
  wrote dev/tmp/ca.key
--- Step 2: generating OIDC issuer signing keypair ---
--- Step 3: writing JWKS ---
  wrote dev/jwks.json
--- Step 4: minting JWT ---
  wrote dev/token
  sub:     system:serviceaccount:polar:git-observer
  iss:     http://localhost:8080
  aud:     polar-cert-issuer.dev
  expires: 3600 seconds from now
--- Step 5: writing config ---
  wrote dev/config.json

=== Dev environment ready. Run in three terminals: ===

  # Terminal 1: JWKS server
  python3 -m http.server 8080 --directory dev/

  # Terminal 2: cert issuer
  cargo run --bin cert-issuer -- --config dev/config.json

  # Terminal 3: init container
  POLAR_SA_TOKEN_PATH=dev/token \
  POLAR_CERT_ISSUER_URL=http://127.0.0.1:8443 \
  POLAR_CERT_DIR=dev/certs \
  cargo run --bin cert-issuer-init

  # Inspect output
  openssl x509 -in dev/certs/cert.pem -text -noout
```

The generated files under `dev/` are not secrets — the keypairs
are test-only and the token has no real cluster privileges. They
are safe to commit as reproducible dev fixtures.

To reset and regenerate everything:

```sh
rm -rf dev/tmp dev/certs dev/token dev/jwks.json dev/config.json
cargo run --bin cert-issuer-setup
```

The token expires after one hour. Re-run `cert-issuer-setup` to
mint a fresh one; existing CA materials are preserved.

### What a successful run looks like

```
Certificate:
    Data:
        Version: 3 (0x2)
        Serial Number: 7b:2c:4f:f6:...
        Signature Algorithm: ecdsa-with-SHA256
        Issuer: CN=Polar Internal CA
        Validity
            Not Before: May  8 05:58:00 2026 GMT
            Not After : May  8 06:28:00 2026 GMT
        Subject:
        Subject Public Key Info:
            Public Key Algorithm: ED25519
        X509v3 extensions:
            X509v3 Subject Alternative Name: critical
                DNS:git-observer.polar.serviceaccount.cluster.local
```

Key things to verify: 30-minute lifetime, Ed25519 public key,
correct DNS SAN, empty Subject (identity is entirely in the SAN),
signed by your dev CA.

## Deploying

### The cert issuer service

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: cert-issuer
  namespace: polar-system
spec:
  replicas: 1
  strategy:
    type: Recreate   # not RollingUpdate — avoid two instances racing on CA materials
  selector:
    matchLabels:
      app: cert-issuer
  template:
    spec:
      serviceAccountName: cert-issuer
      containers:
      - name: cert-issuer
        image: registry.example.com/polar/cert-issuer:v0.1.0
        env:
        - name: CERT_ISSUER_CONFIG
          value: /etc/cert-issuer/config.json
        - name: RUST_LOG
          value: info,cert_issuer=debug
        ports:
        - containerPort: 8443
        volumeMounts:
        - name: config
          mountPath: /etc/cert-issuer/config.json
          subPath: config.json
          readOnly: true
        - name: ca-materials
          mountPath: /etc/cert-issuer
      volumes:
      - name: config
        configMap:
          name: cert-issuer-config
      - name: ca-materials
        persistentVolumeClaim:
          claimName: cert-issuer-ca
```

### Client agent pods

Each agent needs three additions to its pod spec:

```yaml
spec:
  serviceAccountName: git-observer
  volumes:
  - name: sa-token
    projected:
      sources:
      - serviceAccountToken:
          path: token
          audience: polar-cert-issuer.prod   # must match issuer.audience in config
          expirationSeconds: 3600
  - name: certs
    emptyDir: {}

  initContainers:
  - name: cert-handshake
    image: registry.example.com/polar/cert-issuer-init:v0.1.0
    env:
    - name: POLAR_SA_TOKEN_PATH
      value: /var/run/secrets/sa-token/token
    - name: POLAR_CERT_ISSUER_URL
      value: http://cert-issuer.polar-system.svc.cluster.local:8443
    - name: POLAR_CERT_DIR
      value: /var/run/polar/certs
    volumeMounts:
    - name: sa-token
      mountPath: /var/run/secrets/sa-token
      readOnly: true
    - name: certs
      mountPath: /var/run/polar/certs

  containers:
  - name: git-observer
    image: registry.example.com/polar/git-observer:v0.4.2
    volumeMounts:
    - name: certs
      mountPath: /var/run/polar/certs
      readOnly: true
    # No sa-token mount here — the SA token is scoped to the init
    # container only. The workload never holds it.
```

### Init container environment variables

| Variable              | Required | Default                          | Description                          |
|-----------------------|----------|----------------------------------|--------------------------------------|
| `POLAR_CERT_ISSUER_URL` | Yes    | —                                | Base URL of the cert issuer service  |
| `POLAR_SA_TOKEN_PATH`   | No     | `/var/run/secrets/polar/token`   | Path to the projected SA token file  |
| `POLAR_CERT_DIR`        | No     | `/var/run/polar/certs`           | Output directory for cert bundle     |

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
