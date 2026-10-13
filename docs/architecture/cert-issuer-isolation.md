# Isolating cert-issuer: every certificate through the credentials agent

Status, 2026-10-13: **the floor is built in code and tests and is not enabled anywhere; the rest is analysis.** Finding 2 below was confirmed
at the handler level by a red test (`cert-issuer/server/tests/issue_extras_test.rs`, uncommitted until the fix
lands); the rest is from reading the code and the cluster. Written at David's request after the secret-harness run showed that no agent can retrieve a
secret (`RequesterCertificateNotFound`), which traced to agents not obtaining their certificates through the
credentials agent. Supplements `credentials-agent-architecture.md` ("Core services vs. everything else") and
`identity-and-bootstrap-update.md`.

## The invariant David wants

cert-issuer mints Polar's certificates and sits in front of the CA. Nothing may obtain a certificate from it without
passing the controls the credentials agent applies (credential validation, renewal prevention, grants, the ledger
record). The only things that talk to cert-issuer directly are the ones for which that is a hard bootstrap
precondition. This is stated in `credentials-agent-architecture.md`: Cassini and the credentials agent, "the
smallest possible set", and everything else "talks only to the Credentials Agent's observer".

## Where it stands (read from the code)

### Who asks cert-issuer for what today

| Caller | How | What it gets |
|---|---|---|
| `polar-init`, in every pod (22 client certificates, 3 server certificates) | `POST /issue` with the pod's service-account token as the bearer, over TLS, no client certificate | whatever `--cert type:dir:algorithm:extra-sans` asks for |
| the credentials agent consumer's `IssuanceHandler` | `POST /validate`, then `POST /issue`, **with the requesting workload's own token forwarded as the bearer**, over TLS, no client certificate | a client certificate, `CertType::Client` only, with `p:` extras taken from a grant |
| the credentials agent consumer's account flow | `POST /internal/issue` with its own client certificate | account (user) certificates |
| anyone | `GET /ca` | the CA certificate (unauthenticated on purpose) |

The three server certificates are Cassini's (`cassini-ip-svc.polar.svc.cluster.local`), Neo4j's (several names) and
the git server's. Every other workload asks for a client certificate.

### What controls exist

- `/internal/issue` requires a verified mTLS peer on `internal_callers`, which holds only
  `credentials-agent.polar.serviceaccount.cluster.local` (`infra/layers/2-services/cert-issuer/configmap.dhall`).
  Capability (`c:`) extras are refused on `/issue` and so can only be minted there.
- The consumer's issuance flow (`credentials-agent/consume/src/issuance.rs`) classifies the credential, validates it
  with cert-issuer, refuses a second certificate for the same `(identity, instance_binding)` (a pod cannot renew
  itself), reads the grant (a Kubernetes credential needs none; any grant's permissions become `p:` SANs), signs,
  and appends `IssuanceCompleted` with the certificate to the ledger.
- The observer's `POST /issue` relays a request `{csr_pem, credential}` to the consumer over Cassini.

### What does not exist

1. **`polar-init` has no way to go through the credentials agent.** The architecture document describes "a new flag
   pointing it either at `cert-issuer` directly or at the Credentials Agent's observer". `polar-init` reads
   `POLAR_INIT_CERT_ISSUER_URL` and nothing else; the flag is not built. So no agent identity has an
   `IssuanceCompleted` record, which is why `RetrieveSecretHandler` and `DecryptOnBehalfOfHandler`, which look the
   requester's certificate up in the ledger, refuse every agent.
2. **`/issue` is open to any holder of a valid service-account token, and it trusts the extras the caller asks for.**
   After the identity check, the only extras refused are wildcards, IP addresses and `c:`. Everything else is
   accepted and written into the certificate: a name starting `p:` becomes a URI SAN (a permission, with no grant
   behind it), and any other string becomes a DNS SAN. `cert_type` is the caller's too (`Server` as well as
   `Client`). By reading the code, a pod could therefore ask for a certificate carrying permissions it was never
   granted, or a server certificate naming `cassini-ip-svc.polar.svc.cluster.local` or `cert-issuer.polar...`.
   **Confirmed 2026-10-13 against `Handler::handle` with a recording CA** (not on the cluster, and no certificate
   was requested from the live cert-issuer): a token-only caller had a `p:u2s:...` permission signed into a client
   certificate, a `Server` certificate signed naming `cassini-ip-svc.polar.svc.cluster.local`, and a client
   certificate signed naming `cert-issuer.polar.svc.cluster.local`. A plain client certificate and the refusal of
   `c:` extras behave as intended (those two control tests pass).
3. **The consumer's own calls to `/issue` are not authenticated as the credentials agent.** It forwards the
   workload's token on a connection with no client certificate, so cert-issuer sees the workload's token and cannot
   tell the credentials agent from the workload itself. This matters for the fix: restricting `/issue` by token
   identity would refuse the credentials agent's own use of it.
4. **The credentials agent's issuance request cannot carry a certificate type or server names.** `IssuanceRequest`
   is `{csr_pem, credential}` and the handler always issues `CertType::Client`. Cassini, Neo4j and the git server
   need server certificates with DNS names, which this path cannot produce.
5. **No network control.** There is no NetworkPolicy anywhere in the repository, so every pod can reach
   cert-issuer's port.

### Does the dev cluster enforce NetworkPolicy? No.

The `u7s` cluster (`nix-usernetes`, one node, Kubernetes 1.33) is configured with the CNI chain `ptp` (ipMasq),
`host-local`, `tuning` and `portmap`, in `flake.nix` (`cniConfig`, `10-u7s.conflist`). `kube-system` runs CoreDNS and
nothing that enforces policy; no Calico, Cilium, kube-router or similar is installed. The API server serves the
`NetworkPolicy` resource, so **an applied policy would be accepted and silently not enforced**. That is worse than
having none, because it reads as a control. (`flannel` is in the dev shell's package list but the CNI configuration
does not use it.) Candidates that add enforcement to a cluster like this are kube-router in policy-only mode and the
SIG-Network `kube-network-policies` agent; I have not tried either.

## Built: the floor (2026-10-13), off by default

`cert-issuer/server/src/floor.rs` and the handler. Tests: `floor.rs` unit tests, `tests/issue_extras_test.rs`
(handler), `tests/tls_listener_test.rs` (over the real listener, with real client certificates), `tests/config_test.rs`;
each rule was broken on purpose and caught.

- `mtls.bootstrap` in cert-issuer's config is the list of bootstrap identities, each `{workload, cert_type, names}`;
  the workload is the issuer's own identity claim for it (`system:serviceaccount:polar:cassini`), so no cluster domain
  is written. Entries naming a permission, a capability, a wildcard or nothing do not load.
- `/issue` and `/validate`: a caller whose verified mTLS identity is an `internal_callers` entry (the credentials
  agent) is not limited and states the type and names itself (`c:` is still refused on `/issue`); a caller with only
  a token must be a bootstrap identity and gets only its type and names; anything else gets `CALLER_NOT_AUTHORIZED`
  (403) before a CSR is looked at.
- **Absent `mtls.bootstrap` is the old open behaviour**, and cert-issuer says so at every start. Turning it on while
  `polar-init` still asks cert-issuer directly would stop every other workload from starting, so the chart renders
  the list (`values.dhall`: `mtlsBootstrap`) but writes it only when `mtlsEnforceBootstrap` is true, which it is not.
  Enabling it is part of the change that moves `polar-init` onto the observer.
- The credentials agent consumer's cert-issuer clients now all present its own mTLS certificate, which is what lets
  cert-issuer tell its forwarded tokens from a workload's own.

Not built: the `polar-init` observer mode (so nothing can enable the floor yet), the extension of `IssuanceRequest`
for server certificates, NetworkPolicy emission and the canary, and everything under "The model" below.

## The model (David, 2026-10-13)

The static configuration is a **capability floor, not a permissions ceiling.** It is what must be true for Polar to
exist when the credentials agent is not available: a degraded state, meant to be brief and rare. The nominal state
is the credentials agent available, and everything is designed around that. In it, signed policy is authoritative
in either direction and is strictly more expressive than the floor (it may allow what the floor does not, as well as
forbid what the floor allows). This is the rule `policy_authority::admit` already follows: Cedar answers first, and
the floor decides only when there is no answer. Active Directory is the analogy: a node keeps working from its local
policy cache when the domain controller is down, and with the controller it can do strictly more.

Consequences, as proposed (not yet built):

- **No separate capability for widening, no "narrow only" rule.** Whoever holds `c:update-policy` decides, and that
  capability is for people who can be trusted with it. (I had proposed otherwise; withdrawn.) Whether any invariant
  should hold *regardless* of policy, such as a core service's name only ever being signed for that service, is an
  open option, to be added as an arrival-time validator if it is ever wanted, like `validate_source` for policy.
- **cert-issuer cannot depend on Cassini or the scheduler, in any state.** Cassini's certificate comes from it, and
  its health must not depend on a broker that needs its certificates. When it joins the builder it joins as a
  service with no Cassini client.
- **Delivery by push over the relationship that already exists.** The credentials agent is already a verified mTLS
  caller of cert-issuer (`internal_callers`). It would push the same signed `AuthorityUpdate` bytes to an internal
  endpoint; `published_by` is the verified peer. Verification is the existing path (signature, sequence and
  version), with the credentials agent's key **pinned by configuration**, not on first use: this is the CA front.
  The builder creates its authority handle only for Cassini clients today (`if args.with_client`), so a service-mode
  variant would be needed; I have not read the builder closely enough to say how large that is.
- **A local cache, as AD has.** cert-issuer persists the last verified update it received (it has a PVC), and on
  start re-verifies it against the pinned key and uses it. It falls back to the bare floor only on first boot or when
  the cache is gone. Not checked: whether the authority code can load from a cache as it stands.
- **The floor stays**, for the three bootstrap identities and their names, rendered from the catalog. Building it
  first loses nothing: it is what the degraded state runs on and what Cedar answers above.

## Proposed shape

Four layers. The application layer is the real control; the network layer is defence in depth and must not be the
only thing standing between a pod and the CA.

### 1. cert-issuer authenticates its caller, not only the token

- `/issue` and `/validate` accept a request in exactly two cases:
  - the connection presents a client certificate whose identity is on `internal_callers` (the credentials agent
    consumer), carrying a workload token as the credential it is vouching for; or
  - the **token's own identity** is on a short `bootstrap_identities` list and no client certificate is presented:
    the identities that cannot be issued through the credentials agent because they are what it needs to run.
    Candidates: `credentials-agent`, `credentials-agent-observer` (it must have a certificate to reach Cassini at
    all) and `cassini`. David has said Cassini is questionable; the architecture document says it is a hard
    precondition, and the observer's relay path needs a working broker.
- Anything else is refused at the door with `CallerNotAuthorized`, whatever its token says.
- The consumer's cert-issuer client must be built with its own identity (`HandshakeClient::for_trust_mode_with_identity`,
  as the account flow already does) for `/issue` and `/validate`.

### 2. Extras are not the caller's to choose

- On the bootstrap path, extras are limited to a per-identity list in cert-issuer's configuration (for `cassini`,
  its service name; for the credentials agent and observer, none), and `cert_type` is fixed per identity.
- On the vouched path, the credentials agent states the type and the names, from something it holds, not from the
  workload's say-so: permissions come from the grant (already so), server names from a declared allowlist per
  identity (the three server certificates today). Where that allowlist lives is an open question below.
- `p:` extras are refused on `/issue` as `c:` already are, and issued only on the vouched path.

### 3. `polar-init` goes through the observer, except for the bootstrap identities

- A mode (the architecture document's flag) that sends `{csr_pem, credential}` to the observer's `POST /issue`,
  trusting `polar-ca` as it does cert-issuer today. The pod has no certificate yet, so this must be HTTP to the
  observer, which relays over Cassini.
- `IssuanceRequest` gains what a server certificate needs (type, names), or the three server workloads are issued
  by a separate request the consumer checks against the allowlist.
- The catalog's `provides: [... "pinned-ca" ...]` and `cert_issuer_url` become, per workload, either the observer
  or cert-issuer (bootstrap only). The resolver should refuse a workload that names cert-issuer and is not on the
  bootstrap list, so the rule is checked at render time and not only at runtime.
- Availability coupling, accepted in the architecture document: if the credentials agent or Cassini is down, no
  ancillary pod can get a certificate, even with cert-issuer healthy. Startup ordering must wait on the observer.
- Result: every workload except the bootstrap three has an `IssuanceCompleted` record; the harness's retrieve
  works as built; the revocation check in the retrieve handlers means something.

### 4. Network isolation, with a fallback

- **Expected control.** A NetworkPolicy on the cert-issuer pods allowing ingress to its port only from the
  credentials agent consumer, the observer and Cassini (and their `polar-init`, which runs in the same pods), plus
  whatever operator tooling is meant to reach it.
- **Automated where the cluster can enforce it.** A render-time rule emits the policy; an install-time probe
  decides whether the cluster honours it (below).
- **Detected, not assumed.** The installer runs a canary: two pods and a deny-all policy, and checks that the
  connection is actually refused. A cluster that accepts the policy but does not enforce it fails the canary. The
  result is recorded where operators will see it (the target's install report and a ConfigMap), and the target
  declares whether it requires enforcement (`require_network_policy`): refuse to proceed if so, otherwise proceed
  and say plainly that layer 4 is absent.
- **Fallback when there is no enforcement (this dev cluster today).** Layers 1 to 3 carry the whole control:
  the door refuses unauthenticated and unlisted callers, the extras are fixed, and `polar-init` is on the observer.
  The documented residual risk is that any pod can still open a connection to cert-issuer, and what it can do with
  that is limited to the bootstrap identities' tokens and `GET /ca`. The dev cluster could also gain enforcement
  (add a policy engine to `nix-usernetes`), which is the cluster's owner's to decide.

## Order of work, tests first

1. **Confirm finding 2 with a test** against `handler.rs` (a token for identity X asks for `p:` extras and a
   `Server` type): red now, which is the specification for layer 2.
2. Layer 1 and 2 in cert-issuer: caller authentication on `/issue` and `/validate`, `bootstrap_identities`,
   per-identity extras, `p:` refused on `/issue`; the consumer's client gets its identity. Tests: an unlisted
   caller with a valid token is refused; the consumer is accepted; a bootstrap identity gets only its own names; every
   existing issuance test keeps passing.
3. The observer/consumer request grows what server certificates need; the consumer's allowlist; tests with the three
   current server workloads.
4. `polar-init` observer mode; catalog `via`; resolver rule; startup ordering.
5. Move the workloads over in groups, ending with Neo4j and the git server (server certificates), verifying each on
   the cluster; the harness is the check that retrieve works afterwards.
6. NetworkPolicy emission, the canary, `require_network_policy`; document the control and the fallback in
   `INFRA.md`.

Doing 2 before 4 breaks every workload's startup until 4 is done: the order above is one change set to deploy
together, or 4's observer mode first behind a flag with 2 following.

## Decisions (David, 2026-10-13)

- **Bootstrap set, for now:** `credentials-agent`, `credentials-agent-observer`, `cassini`. Tightening it is a
  follow-up.
- **The dev cluster runs on the no-enforcement fallback** for now; adding a policy engine to `nix-usernetes` is a
  follow-up patch to u7s.
- **Starting point:** finding 2, as a red test, before any change.

## Open questions

1. ~~Bootstrap set~~ decided above. Whether the observer could later be issued by the consumer over a path that
   does not need Cassini (it cannot today) is the follow-up that would tighten it.
2. **Where the server-name allowlist lives.** Under the model above: the floor (bootstrap identities and their
   names) in cert-issuer's configuration, rendered from the catalog; everything beyond it in the signed policy
   cert-issuer enforces itself once it hosts the authority. Not a signed section held by the credentials agent.
3. **`/validate`.** Does it also move behind caller authentication? It reveals the normalized identity of any valid
   token; it signs nothing, so it is a smaller concern than `/issue`.
4. **Grants for workloads.** Today a Kubernetes credential needs none. Do you want workload certificates to carry
   `p:` permissions from grants (the mechanism exists), and is that what Neo4j's per-identity access is meant to read?
5. ~~The dev cluster's enforcement~~ decided above: the fallback now, a u7s patch later.

## What I did not check

- Whether finding 2 is exploitable end to end (no test was run, and no certificate was requested).
- What Cassini does with the `p:` SANs it sees, and what Neo4j does with them (whether either grants access by them).
- Whether any workload depends on being able to choose its own extras (the 22 client certificates ask for none; the
  three server certificates ask for names, listed above).
- The CNI options for the dev cluster beyond naming them.
