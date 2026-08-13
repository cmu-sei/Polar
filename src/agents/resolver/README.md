# OCI Resolver

The resolver is a Polar agent that turns *discovered* image references into *resolved* manifest digests. It subscribes to `PROVENANCE_DISCOVERY` on Cassini, and for each `OCIArtifactDiscovered` event it parses the image reference, resolves credentials, performs a manifest pull against the registry, and republishes an `OCIArtifactResolved` event carrying the digest, the serialized manifest, and the normalized registry hostname. Kubernetes-sourced discoveries are additionally forwarded to `KUBERNETES_RESOLUTION_EVENTS` so the k8s consumer can attach the resolution to the pod container it came from.

It is an OCI *client*, not a registry mirror and not a scanner. It pulls manifests only — never blobs, never layers. The unit of work is one network round-trip per discovered reference, and the output is a digest plus the manifest bytes.

Everything is `ractor` actors. `ResolverSupervisor` owns the Cassini TCP client and constructs the `oci-client` instance once the broker registration completes; `ResolverAgent` is spawned linked to the supervisor and does the per-event work. A failure in the agent takes down the supervisor, which is intentional — the resolver is stateless and restarting is cheaper than reasoning about a half-configured client.

## Why the configuration exists

Two things forced it. First, image references from the Kubernetes processor arrive exactly as they were written into the pod spec, which means a large fraction of them have no registry component at all — `polar-nu-init:latest`, `rancher/klipper-helm:v0.9.14-build20260210`. Every other OCI client in existence treats these as Docker Hub shorthand. The resolver used to short-circuit and drop them (see cmu-sei/Polar#219); it now qualifies them against a configurable default registry, with the old behaviour available as an explicit policy for air-gapped deployments where an outbound Docker Hub attempt is itself the finding. Second, testing against a local `registry:2` required either standing up a PKI or hardcoding a plaintext host into the source, which is what cmu-sei/Polar#234 is about.

## Authoring configuration

The shape of the configuration is defined in Dhall under `config/`. Dhall is the authoring and validation layer; the resolver itself reads YAML.

- `config/schema.dhall` — the types, the defaults, and the commentary explaining what each knob actually does to the underlying `oci-client`. Read this first.
- `config/render.dhall` — lowers the schema types to the flat record that becomes YAML.
- `config/example.dhall` — a worked example. Copy this for a new environment.
- `config/defaults-to-yaml.dhall` and `config/default.yaml` — the rendered schema defaults, committed and embedded in the binary.

To produce a config file:

```
dhall-to-yaml --preserve-null --file config/example.dhall > oci-resolver.yaml
```

Every file here that is meant to be handed to `dhall-to-yaml` ends with `./render.dhall` applied to the config value. That is load-bearing. A bare `S.Config::{...}` will render happily and produce structurally valid YAML that the resolver rejects — `tls.protocol` as an untagged list instead of `tls.mode` plus `tls.plaintextHosts`, and `unqualifiedRefs` as a nested map instead of the flat `registry:` section. When you add a new environment config, apply the renderer on the last line and keep the unlowered value bound to a `let`, so there is nothing in the directory that can be rendered into an invalid config by accident.

`--preserve-null` matters. Without it `dhall-to-yaml` omits `None` fields entirely rather than emitting `null`. The Rust loader tolerates either — every field carries `#[serde(default)]` — but emitting the keys keeps the rendered file self-documenting for operators who will never open the Dhall.

Point the resolver at the result:

```
POLAR_OCI_RESOLVER_CONFIG=/etc/polar/oci-resolver.yaml
```

If the variable is unset, the embedded defaults are used and this is logged at `INFO`. If it is set but the file is missing, unparseable, or fails validation, the process panics at startup. That asymmetry is deliberate: a resolver silently running with different transport policy than the operator believes is a worse outcome than one that refuses to boot.

## Why Dhall renders to YAML instead of being read directly

Dhall's sum types are the point — they make invalid states unrepresentable at authoring time. You cannot specify a plaintext exception list while also declaring TLS everywhere, because those are two alternatives of one union. But `dhall-to-yaml` **erases union tags** on serialization: `Protocol.HttpsExcept ["a"]` comes out as the bare list `["a"]`, indistinguishable from any other list, and `dhall-json` has no `--preserve-union-tags` flag (that is `dhall-to-yaml-ng`). So the union cannot survive the trip.

The lowering in `render.dhall` therefore emits an explicit discriminator — `tls.mode`, `registry.unqualifiedRefs` — alongside the payload fields, and `TryFrom<wire::Config> for ResolverConfig` reconstructs the sum type on load. Nothing downstream of that conversion ever sees "this field is meaningful only when that other field has a particular value." Because YAML is hand-editable and Dhall is not in the runtime path, the Rust side re-validates rather than trusting the generator.

## Configuration reference

### `registry`

Controls what happens to references with no registry component.

```yaml
registry:
  unqualifiedRefs: qualify      # or "skip"
  defaultRegistry: docker.io
  defaultNamespace: library
```

`qualify` prepends `defaultRegistry`, and prepends `defaultNamespace` as well when the repository is a single segment. So `nginx:1.25` becomes `docker.io/library/nginx:1.25`, while `rancher/klipper-helm:v0.9.14` already has two segments and only gains the registry. This mirrors Docker's `library/` rule, which is Docker-Hub-specific; if you point `defaultRegistry` at a Harbor or Artifactory instance you probably want a different namespace, or `null` for none. (Harbor's default project also happens to be called `library`. Coincidence, not standard.)

`skip` restores the pre-#219 behaviour: unqualified references are dropped before any network call. Set `defaultRegistry` and `defaultNamespace` to `null` when you use it — leaving them populated is a startup error, because a field that silently does nothing is worse than a refusal to start.

Qualification is performed on the reference *string*, before parsing. This is not an accident. `oci-spec` hardcodes `docker.io` and an implicit `library/` insertion inside `split_domain`, so letting it parse first would make a configured namespace unreachable whenever the configured registry happened to be Docker Hub.

`defaultRegistry` is validated by parsing `<host>/probe` and checking the registry round-trips. That reuses the vetted grammar rather than reimplementing hostname rules, and it catches the common mistake of writing a bare word: `myregistry` would otherwise be silently reinterpreted as the first path segment of a Docker Hub repository.

### `tls`

```yaml
tls:
  mode: https-except            # https | https-except | http-insecure
  plaintextHosts:
    - registry.local-registry.svc.cluster.local:5000
  extraRootCertificates:
    - /etc/polar/pki/proxy-ca.pem
  exclusiveTrustStore: false
```

`https` is TLS for everything and is the default. `https-except` is TLS for everything except the listed hosts. `http-insecure` disables TLS globally, including for credentials on the wire, and exists for integration tests against a throwaway `registry:2`. It logs a `WARN` on every startup and should never appear in a deployed configuration.

The entries in `plaintextHosts` are compared by **exact string equality** against `Reference::resolve_registry()`, which is the post-normalization host including the port. Three consequences that will bite you, and which the loader now rejects at startup rather than silently ignoring:

- Docker Hub must be spelled `index.docker.io`, because `resolve_registry()` rewrites `docker.io` to it. Writing `docker.io` here is a hard error. You almost certainly do not want plaintext Docker Hub anyway.
- A registry on a non-443 port must include the port. Omitting it does nothing.
- A scheme or a trailing path can never match, so `http://registry.local:5000` is a hard error.

`extraRootCertificates` are PEM files added on top of the platform roots. If `PROXY_CA_CERT` is present in the environment it is *appended* to this list at startup rather than replacing it. `exclusiveTrustStore` maps to `ClientConfig::tls_certs_only` and discards the platform and built-in roots entirely, trusting only what you listed; setting it with an empty certificate list is a startup error because the client would trust nothing at all.

There is deliberately no `acceptInvalidCertificates` knob, even though `oci-client` exposes one. Plaintext HTTP announces itself in the config file, in the logs, and on the wire. TLS with verification disabled looks identical to real TLS everywhere a human or a dashboard would look, while providing none of the guarantees. If you need a local registry, use `https-except`. If you are behind an inspecting proxy, add its CA to `extraRootCertificates`.

### `auth`

```yaml
auth:
  dockerConfig: null
  anonymousFallback: true
```

Credentials come from `docker-credential`, which reads `config.json` and any configured credential helpers. `dockerConfig` overrides the directory; `null` defers to the library's own search (`$DOCKER_CONFIG`, then `~/.docker`).

The resolver tries several credential keys per registry, in priority order, because Docker's own key format is inconsistent. Docker Hub is special-cased: `resolve_registry()` yields `index.docker.io` while Docker writes credentials under the legacy `https://index.docker.io/v1/`. Without that alias, defaulting unqualified references to Docker Hub would authenticate anonymously against the most aggressively rate-limited registry on the internet while valid credentials sat on disk. `http://` variants are emitted only for hosts where the transport policy actually permits plaintext.

`anonymousFallback: false` turns "no credential found" into a resolution error instead of an anonymous attempt. That makes credential misconfiguration loud, at the cost of losing public registries entirely.

Docker identity tokens are recognized and skipped. They are refresh tokens for the registry's token endpoint, not bearer tokens for the distribution API, so passing one through as `RegistryAuth::Bearer` would fail. Supporting them properly needs an OAuth2 refresh exchange, which is not implemented.

### `http`

```yaml
http:
  connectTimeoutMs: 5000
  readTimeoutMs: 30000
  httpsProxy: null
  httpProxy: null
  noProxy: null
  userAgent: polar-oci-resolver
```

Straight passthrough to `ClientConfig`. The connect timeout default is low on purpose: with qualification enabled, an air-gapped cluster will attempt to reach `index.docker.io` once per locally-loaded image per restart, and the timeout is what bounds that cost. There is no negative cache yet, so if that cost matters in your environment, use `unqualifiedRefs: skip` for now.

`userAgent` is a `String` here but `ClientConfig::user_agent` is `&'static str`, so it is leaked once at startup. A single bounded leak is the honest cost of that upstream API.

### `mirrors`

```yaml
mirrors:
  - upstream: docker.io
    mirror: harbor.corp.internal
```

Pull-through mirroring in the containerd style: requests for `upstream` are sent to `mirror` with `?ns=<upstream>` appended. This maps onto `Reference::set_mirror_registry`, which upstream marks `#[doc(hidden)]` and explicitly exempts from semver guarantees. If you enable mirrors in production, pin `oci-client` with `=`. If you would rather not carry that risk, leave the list empty.

## Build and test

Minimum `oci-client` is **0.16** — `ClientConfig::tls_certs_only`, which backs `exclusiveTrustStore`, does not exist in 0.15.

`config/default.yaml` is generated and committed, then embedded via `include_str!` and asserted equal to `wire::Config::default()` in the test suite. That test is the gate that keeps the Dhall schema and the hand-written Rust defaults from drifting. CI additionally re-renders and diffs:

```
dhall-to-yaml --preserve-null --file config/defaults-to-yaml.dhall \
  | diff -u config/default.yaml -
```

If you change a default, change it in `schema.dhall`, re-render, and commit both.

### Nix

`config/default.yaml` is a non-`.rs` file that the build depends on at compile time. Crane's `cleanCargoSource` and most hand-rolled `lib.cleanSource` filters strip it, at which point `include_str!` fails, `config.rs` fails to compile, and every use of `ResolverConfig` in `main.rs` produces a cascade of unrelated-looking type errors — commonly `str` is not `Sized` at each `tracing` macro call site. `cargo build` from a working tree never reproduces this, because the file is sitting right there.

Widen the source filter:

```nix
src = lib.cleanSourceWith {
  src = ./.;
  filter = path: type:
    (lib.hasInfix "/config/" path)
    || (craneLib.filterCargoSources path type);
};
```

When Nix and local Cargo disagree, read the *head* of the build log rather than the tail — `nix build -L 2>&1 | head -120`. The tail is almost always error-recovery cascade. The other common divergence is lockfile drift: Nix builds offline and pinned, so reproduce with `cargo clean && cargo build --locked` before assuming Nix is at fault.

## Known gaps

There is no negative cache for failed resolutions, so a reference that cannot be resolved is retried on every discovery event. Identity-token credential flows are unsupported. Broker and Cassini settings still come from the environment rather than this file; unifying them means deciding precedence against the existing variables, which is a separate change. And per cmu-sei/Polar#219, a qualified-by-assumption reference is currently indistinguishable in the emitted event from one that was fully qualified in the pod spec — the provenance gap should surface as a property on the graph node, and does not yet.
