# Cert Issuer

Short-lived X.509 client certificates for Polar agents, minted at
pod startup against a configured OIDC issuer (Kubernetes projected
service account tokens in v1) and signed by a Smallstep CA.

See [the architecture specification](../../../docs/architecture/cert-issuer-architecture.md) for the full
design rationale, threat model, and v2 directions.

## Workspace Layout

```
cert-issuer-common/   Shared wire types (request/response/outcomes)
cert-issuer/          The service itself (library + binary)
cert-issuer-init/     The polar-nu-init handshake binary
dhall/                Config schema and example environment configs
```

## Building

The workspace builds with stable Rust 1.75+. There are no native
build dependencies beyond a working `cargo` and a C linker (for
`ring` and `reqwest`'s TLS backend, both of which build pure-Rust
crypto by default).

```sh
cargo build --package cert-issuer --package cert-issuer-init --release
```

Three binaries land in `target/release/`:

- `cert-issuer` — the service
- `cert-issuer-init` — the init container client
- (no third binary; `cert-issuer-common` is types-only)

For development:

```sh
cargo build --workspace
cargo test --workspace
```

The unit tests have no external dependencies. Integration tests
use `wiremock` to stand up in-process HTTP servers and require no
network access. Tests should complete in under 30 seconds on a
modern laptop; if a test hangs longer, it's almost certainly the
RSA keypair generation in OIDC test setup, which dominates first-run
time but is cached across tests within a single test binary.

## Configuration

Config is provided as a JSON file referenced by the
`CERT_ISSUER_CONFIG` environment variable. The file's schema is
defined in [`dhall/schema.dhall`](dhall/schema.dhall).

We use Dhall as the source of truth and render JSON for the binary
because Dhall gives us type-checked composition across environments
without forcing the binary to depend on a Dhall parser. The flow:

```sh
dhall-to-json --file dhall/dev.dhall > /tmp/cert-issuer.json
CERT_ISSUER_CONFIG=/tmp/cert-issuer.json ./target/debug/cert-issuer
```

In production this rendering happens at deploy time — the rendered
JSON ends up in a Kubernetes Secret or ConfigMap mounted into the
pod, with `CERT_ISSUER_CONFIG` pointing at the mount path.

### Writing a per-environment config

The schema defines defaults for everything that has a sensible
default. A minimal per-environment config only needs to set the
fields that vary:

```dhall
let schema = ./schema.dhall
in  schema.ServiceConfig::{
    , issuer = schema.IssuerConfig::{
      , issuer = "https://kubernetes.default.svc"
      , audience = "polar-cert-issuer.prod"
      , jwks_uri = Some "https://kubernetes.default.svc/openid/v1/jwks"
      }
    , ca = schema.CaConfig::{
      , url = "https://step-ca.polar-system.svc.cluster.local:9000"
      , provisioner = "cert-issuer"
      }
    }
```

See [`dhall/dev.dhall`](dhall/dev.dhall) for a complete worked
example with comments.

### Required fields

Three fields have no defaults and must be set per-environment:

- `issuer.issuer` — the OIDC issuer URL the cert issuer trusts
- `issuer.audience` — the audience tokens must claim
- `ca.url` and `ca.provisioner` — where step-ca is and which
  provisioner to use

The Rust validator at startup rejects empty values for these
fields, so a misconfigured config fails fast rather than producing
a service that quietly accepts any token.

### Provisioner key

step-ca authorizes signing requests via a one-time token (OTT)
signed by the provisioner's private key. The cert issuer holds
this key and signs a fresh OTT per request.

The key path is set in `ca.provisioner_key_path`. The default is
`/etc/cert-issuer/provisioner.key`. The expected format is PKCS#8
PEM, and the supported algorithms are EdDSA (Ed25519) and ES256 —
v1 doesn't support RSA provisioner keys.

To generate a new EdDSA provisioner key with step-cli:

```sh
step crypto keypair provisioner.pub provisioner.key --kty OKP --crv Ed25519
```

Register the public key with step-ca (consult step-ca's docs for
your CA configuration), then mount the private key into the cert
issuer's pod as a Kubernetes Secret.

### TLS

v1 of the cert issuer **does not terminate TLS itself**. The binary
serves plain HTTP on the configured `bind_addr`, and TLS is the
responsibility of whatever fronts it — typically a service mesh
sidecar, an ingress controller, or a sidecar proxy.

This is a deliberate v1 simplification. Adding native TLS via
`axum-server` is a small change but requires a TLS cert for the
cert issuer itself, which has a chicken-and-egg problem the
architecture spec discusses (Section 9). v1 avoids this by
deferring to an existing TLS-terminating layer.

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

A minimal Deployment looks like:

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: cert-issuer
  namespace: polar-system
spec:
  replicas: 1
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
          mountPath: /etc/cert-issuer
          readOnly: true
        - name: provisioner-key
          mountPath: /etc/cert-issuer/provisioner.key
          subPath: provisioner.key
          readOnly: true
      volumes:
      - name: config
        configMap:
          name: cert-issuer-config
      - name: provisioner-key
        secret:
          secretName: cert-issuer-provisioner-key
          defaultMode: 0400
```

The ConfigMap holds the rendered JSON config. The Secret holds the
provisioner private key. Generate both with the same Dhall
rendering pipeline you use for any other Polar service.

A NetworkPolicy restricting traffic to the cert issuer's pod is
strongly recommended — only the agents that need to attest should
be able to reach `:8443`. A representative NetworkPolicy is in
[`deploy/networkpolicy.yaml`](deploy/networkpolicy.yaml) (TODO:
add this; current state is reference-only).

### Updating client agents

Each agent that consumes a cert from this service needs three
changes to its pod spec:

1. **Add a projected service account token volume** with the
   audience set to the cert issuer's configured audience. This is
   the bootstrap credential the init container presents.
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
    # NOTE: no projected SA token volume mounted here. The token
    # belongs to the init container only; the workload container
    # should never see it. This is the security property the init
    # container model gives you over self-attestation.
```

The `audience` field on the projected SA token is the
single-most-common deployment misconfiguration. If it's missing
or wrong, the cert issuer rejects the token with
`INVALID_AUDIENCE` and the init container exits non-zero. Check
this first when an agent fails to start.

### Bootstrap order

At cluster bring-up the dependency chain is:

1. step-ca must be running (the trust root).
2. cert-manager (or whatever provisions the cert issuer's *own*
   serving cert, if you've added TLS) must be running.
3. The cert issuer's pod can start.
4. Cassini and the other agents can start. Each goes through the
   init container handshake to obtain its cert.

The cert issuer has no Polar-internal dependencies — its only
runtime dependencies are the Kubernetes API server's OIDC issuer
endpoint and step-ca itself. Both are below the Polar stack in
the dependency graph, so there's no cycle.

## Test Layout

The test suite is organized so that each concern has its own file
and the tests in that file pin down a specific category of
behavior. Reading the test file alone should give you a useful
picture of what the module does and what its boundaries are.

### `cert-issuer/tests/`

| File | What it pins down |
|------|-------------------|
| `config_test.rs` | Config validation rejects forbidden algorithms (HS256, "none") at load time, not at validation time |
| `oidc_test.rs` | Token validation: signature, audience, issuer, expiry, algorithm restrictions, JWKS caching with TTL bounds |
| `csr_test.rs` | CSR parsing, single-SAN enforcement, identity matching is exact-string |
| `handler_unit_test.rs` | Outcome-to-status-code mapping and wire serialization are stable |
| `handler_integration_test.rs` | End-to-end ordering: CA is never called when upstream validation fails. Session IDs are unique per request |

### `cert-issuer-init/tests/`

| File | What it pins down |
|------|-------------------|
| `handshake_test.rs` | HTTP client correctly sends bearer token, parses success and error responses, distinguishes unreachable from rejected |
| `keypair_test.rs` | Each call generates a distinct keypair; CSR is parseable by the cert issuer's parser; empty identity is rejected |
| `output_test.rs` | Three files written, key file is mode 0640, writes are atomic, existing files overwritten on re-run |

## What's NOT Tested at This Layer

These are deliberately deferred to either later phases or to
end-to-end testing in a real cluster:

- TLS termination on the cert issuer's HTTPS endpoint
  (added when v2 lifts the "TLS is upstream's job" simplification;
  tested via real TLS in staging).
- Smallstep CA integration
  (the `CaClient` trait is mocked in unit tests; the real
  `StepCaClient` is tested against a `step-ca` instance in CI).
  Notably, the wire format in `ca.rs` is best-effort recall of
  step-ca's API and is verified against the real CA in CI, not by
  unit tests.
- Cassini event publication
  (Phase B; integration test against a real Cassini broker).
- Prometheus metrics emission
  (covered by smoke tests, not unit tests; metric values are too
  brittle for unit-test assertions).
- The init container binary's exit codes
  (a small `main.rs` shim wraps the library; its behavior is
  tested via subprocess invocation in CI).

## v2 Directions

See Section 12 of the architecture spec for the full v2 backlog.
The high-leverage items are:

- **Cosign signature verification** of the requesting workload's
  image, gating cert issuance on supply-chain provenance.
- **Workload-image attestation via downward API** — closing the
  gap where a legitimate init container is paired with an unsigned
  workload image.
- **Multi-issuer OIDC support** for hybrid environments
  (GitLab CI, SPIFFE/SPIRE, AWS IRSA).
- **Bare-metal targets** via TPM or SPIRE node attestation.

Each of these has explicit trigger conditions in the spec — they
get added when there's a concrete reason, not speculatively.

## Contributing

The project follows a TDD discipline: each module's tests pin down
the behavior contract, and changes that break that contract should
either fix the tests deliberately (with a comment explaining what
contract is changing and why) or fix the code so the tests pass
again.

When adding a new module, write the test file first, leave the
implementation as `todo!()` stubs, and only then fill in the
implementation. The test file's role is to specify the module's
contract; the implementation's role is to satisfy it.
