# Cert Issuer

Short-lived X.509 client certificates for Polar agents, minted at
pod startup against a configured OIDC issuer (Kubernetes projected
service account tokens in v1) and signed by an in-process CA backed
by [`rcgen`](https://crates.io/crates/rcgen).

See [`architecture-spec.md`](architecture-spec.md) for the full
design rationale, threat model, and v2 directions.

## Workspace Layout

```
.
├── agent
│   ├── Cargo.toml
│   ├── conf
│   │   ├── dev.dhall
│   │   └── schema.dhall
│   ├── issuer-config.json
│   ├── src
│   │   ├── ca.rs
│   │   ├── config.rs
│   │   ├── lib.rs
│   │   ├── main.rs
│   │   ├── oidc.rs
│   │   └── telemetry.rs
│   └── tests
│       ├── config_test.rs
│       └── oidc_test.rs
├── common
│   ├── Cargo.toml
│   └── src
│       └── lib.rs
├── init
│   ├── Cargo.toml
│   ├── src
│   │   ├── handshake.rs
│   │   ├── keypair.rs
│   │   ├── lib.rs
│   │   └── output.rs
│   └── tests
│       ├── handshake_test.rs
│       ├── keypair_test.rs
│       └── output_test.rs
└── README.md

10 directories, 23 files
```

## Building

The workspace builds with stable Rust 1.75+. There are no native
build dependencies beyond a working `cargo` and a C linker.

```sh
cargo build --workspace --release
```

Two binaries land in `target/release/`:

- `cert-issuer` — the service
- `cert-issuer-init` — the init container client

For development:

```sh
cargo build --workspace
cargo test --workspace
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

| Cert file | Key file | Behavior                        |
|-----------|----------|---------------------------------|
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
from automatically is "neither file exists" — which is the only
state where regeneration is unambiguously the right action.

### What this means operationally

For first-run on a fresh volume: the cert issuer generates
materials, writes them with mode 0600 on the key, and proceeds.
The CA cert is logged at INFO; the CA key never leaves the
process.

For steady state: the materials persist on the configured volume
and the cert issuer loads them on each restart. The CA root has a
10-year validity by default, and rotation is operator-driven (see
below).

For volume corruption / wrong mount: the cert issuer fails to
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

If you must rotate the CA, do so by:

1. Distributing the new CA cert to all clients alongside the old
   one, so clients accept either during the transition window.
2. Wiping the cert issuer's volume.
3. Restarting the cert issuer (which bootstraps a fresh CA).
4. Once all certs issued under the old root have expired
   (`default_lifetime` after step 3), retire the old cert from
   clients.

Cross-signing for zero-downtime rotation is a v1.x addition
(currently the implementation supports loading exactly one CA at
a time).

### Distributing the CA cert to clients

The CA cert is public — clients verifying issued certs need it as
a trust root. After first run, copy it out of the volume:

```sh
# Local development
cat ./tmp/ca.crt

# Kubernetes
kubectl exec -n polar-system deploy/cert-issuer -- cat /etc/cert-issuer/ca.crt
```

Distribute it as a ConfigMap mounted into client pods, or via
whatever trust-distribution mechanism the rest of your cluster
uses.

## Configuration

Config is provided as a JSON file referenced by the
`CERT_ISSUER_CONFIG` environment variable. The file's schema is
defined in [`dhall/schema.dhall`](dhall/schema.dhall).

We use Dhall as the source of truth and render JSON for the binary
because Dhall gives us type-checked composition across environments
without forcing the binary to depend on a Dhall parser.

```sh
dhall-to-json --file dhall/dev.dhall > /tmp/cert-issuer.json
CERT_ISSUER_CONFIG=/tmp/cert-issuer.json ./target/debug/cert-issuer
```

A minimal per-environment config:

```dhall
let schema = ./schema.dhall
in  schema.ServiceConfig::{
    , issuer = schema.IssuerConfig::{
      , issuer = "https://kubernetes.default.svc"
      , audience = "polar-cert-issuer.prod"
      , jwks_uri = Some "https://kubernetes.default.svc/openid/v1/jwks"
      }
    , ca = schema.CaConfig::{
      , ca_cert_path = "/etc/cert-issuer/ca.crt"
      , ca_key_path = "/etc/cert-issuer/ca.key"
      }
    }
```

See [`dhall/dev.dhall`](dhall/dev.dhall) for a complete example
with comments.

### Required fields

Two fields have no defaults and must be set per-environment:

- `issuer.issuer` — the OIDC issuer URL the cert issuer trusts
- `issuer.audience` — the audience tokens must claim

The Rust validator at startup rejects empty values for these
fields. Misconfigured configs fail fast.

### TLS

v1 of the cert issuer **does not terminate TLS itself**. The binary
serves plain HTTP on the configured `bind_addr`, and TLS is the
responsibility of whatever fronts it — typically a service mesh
sidecar, an ingress controller, or a sidecar proxy.

If you're running this outside Kubernetes and need TLS, the
fastest path is a Caddy or nginx reverse proxy in front, with the
cert issuer bound to localhost only.

## Deploying

The deployment story has two layers: deploying the cert issuer
service itself, and updating client agent pods to use the init
container model.

### Deploying the cert issuer service

The service runs as a single Kubernetes Deployment. v1 is
single-instance-with-rapid-restart per the architecture spec —
in-process state means no horizontal replication for v1.

A representative Deployment:

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: cert-issuer
  namespace: polar-system
spec:
  replicas: 1
  strategy:
    # Recreate (not RollingUpdate) — we don't want two cert issuer
    # pods racing on the same CA materials directory.
    type: Recreate
  selector:
    matchLabels:
      app: cert-issuer
  template:
    metadata:
      labels:
        app: cert-issuer
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

The ConfigMap holds the rendered JSON config. The
PersistentVolumeClaim holds the CA cert and key — written by the
cert issuer on first run, persisted across pod restarts. The
config's `ca_cert_path` and `ca_key_path` should point at locations
inside this mount (e.g. `/etc/cert-issuer/ca.crt` and
`/etc/cert-issuer/ca.key`).

The PVC's storage class must give the pod read/write access. The
cert issuer creates parent directories as needed, so you can mount
an empty volume.

A NetworkPolicy restricting traffic to the cert issuer's pod is
strongly recommended — only the agents that need to attest should
be able to reach `:8443`.

### Updating client agents

Each agent that consumes a cert from this service needs three
changes to its pod spec:

1. **Add a projected service account token volume** with the
   audience set to the cert issuer's configured audience.
2. **Add an `emptyDir` volume** for cert/key/CA chain sharing
   between the init container and the workload container.
3. **Add the `polar-nu-init` init container** with the projected
   token and the emptyDir mounted, configured to call the cert
   issuer with the agent's expected identity.

A representative pod spec fragment:

```yaml
spec:
  serviceAccountName: git-observer
  volumes:
  - name: sa-token
    projected:
      sources:
      - serviceAccountToken:
          path: token
          # MUST match the cert issuer's configured audience.
          audience: polar-cert-issuer.prod
          expirationSeconds: 3600
  - name: certs
    emptyDir: {}
  initContainers:
  - name: cert-handshake
    image: registry.example.com/polar/cert-issuer-init:v0.1.0
    env:
    - name: CERT_ISSUER_ADDR
      value: https://cert-issuer.polar-system.svc.cluster.local:8443
    - name: WORKLOAD_IDENTITY
      value: system:serviceaccount:polar:git-observer
    - name: SA_TOKEN_PATH
      value: /var/run/secrets/sa-token/token
    - name: OUTPUT_DIR
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
```

The `audience` field on the projected SA token is the
single-most-common deployment misconfiguration. If it's missing
or wrong, the cert issuer rejects the token with
`INVALID_AUDIENCE` and the init container exits non-zero.

## Local Development

```sh
# 1. Render the dev config (paths point at ./tmp/ca.{crt,key})
mkdir -p ./tmp
dhall-to-json --file dhall/dev.dhall > ./tmp/cert-issuer.json

# 2. Run the cert issuer. First run will bootstrap the CA into ./tmp.
CERT_ISSUER_CONFIG=./tmp/cert-issuer.json \
  RUST_LOG=debug \
  cargo run --bin cert-issuer

# 3. In another terminal, stand up an OIDC stub that serves a JWKS
#    at http://localhost:8080/jwks.json and lets you mint signed JWTs.

# 4. Mint a token, generate a CSR, POST to /issue:
#    curl -X POST http://127.0.0.1:8443/issue \
#      -H "Authorization: Bearer $TOKEN" \
#      -d "{\"csr_pem\": ...}"
```

A `local-test.sh` script that automates steps 3-4 is the
recommended next step for the development workflow.

To start over with a fresh CA:

```sh
rm -rf ./tmp/ca.crt ./tmp/ca.key
# Next run will bootstrap a new pair.
```

To inspect the bootstrapped CA cert:

```sh
openssl x509 -in ./tmp/ca.crt -text -noout
```

## Test Layout

The test suite is organized so that each concern has its own file
and the tests in that file pin down a specific category of
behavior. Reading the test file alone should give you a useful
picture of what the module does and what its boundaries are.

### `agent/tests/`

| File | What it pins down |
|------|-------------------|
| `config_test.rs` | Config validation rejects forbidden algorithms (HS256, "none") at load time |
| `oidc_test.rs` | Token validation: signature, audience, issuer, expiry, algorithm restrictions, JWKS caching with TTL bounds |
| `csr_test.rs` | CSR parsing, single-SAN enforcement, identity matching is exact-string |
| `handler_unit_test.rs` | Outcome-to-status-code mapping and wire serialization are stable |
| `handler_integration_test.rs` | End-to-end ordering: CA is never called when upstream validation fails. Session IDs are unique per request |

CA tests (issuance, bootstrap, the load-or-bootstrap state machine,
key permission checks, cert-key matching) live in `agent/src/ca.rs`
as `#[cfg(test)] mod tests`.

### `init/tests/`

| File | What it pins down |
|------|-------------------|
| `handshake_test.rs` | HTTP client correctly sends bearer token, parses success and error responses, distinguishes unreachable from rejected |
| `keypair_test.rs` | Each call generates a distinct keypair; CSR is parseable by the cert issuer's parser; empty identity is rejected |
| `output_test.rs` | Three files written, key file is mode 0640, writes are atomic, existing files overwritten on re-run |

## What's NOT Tested at This Layer

These are deliberately deferred to either later phases or to
end-to-end testing in a real cluster:

- TLS termination on the cert issuer's HTTPS endpoint.
- Cassini event publication (Phase B).
- Prometheus metrics emission (smoke tests, not unit tests).
- The init container binary's exit codes (subprocess invocation
  in CI).

## v2 Directions

See Section 12 of the architecture spec for the full v2 backlog.

- **Cosign signature verification** of the requesting workload's
  image, gating cert issuance on supply-chain provenance.
- **Workload-image attestation via downward API**.
- **Multi-issuer OIDC support** for hybrid environments.
- **Bare-metal targets** via TPM or SPIRE node attestation.
- **CA cross-signing** for zero-downtime rotation.

Each has explicit trigger conditions in the spec — they get added
when there's a concrete reason, not speculatively.
