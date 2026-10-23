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

# Isolating cert-issuer: every certificate through the credentials agent

Status, 2026-10-13: **the floor is built in code and tests and is not enabled anywhere; the rest is analysis.** Finding 2 below was confirmed
at the handler level by a red test (`cert-issuer/server/tests/issue_extras_test.rs`, uncommitted until the fix
lands); the rest is from reading the code and the cluster. Written at the maintainer's request after the secret-harness run showed that no agent can retrieve a
secret (`RequesterCertificateNotFound`), which traced to agents not obtaining their certificates through the
credentials agent. Supplements `credentials-agent-architecture.md` ("Core services vs. everything else") and
`identity-and-bootstrap-update.md`.

## The invariant wanted

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

The development cluster (one node, Kubernetes 1.33) is configured with the CNI chain `ptp` (ipMasq),
`host-local`, `tuning` and `portmap`. `kube-system` runs CoreDNS and
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

## The model (2026-10-13)

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
  its health must not depend on a broker that needs its certificates. *Revised 2026-10-23 (the maintainer): no
  dependency, but it may speak over Cassini.* Once Cassini is up, cert-issuer may be its client, and at nominal the
  services that talk to it, the credentials agent among them, do so over the same channels as everything else. It
  starts, serves and stays healthy without Cassini. (It said before: "When it joins the builder it joins as a service
  with no Cassini client.") See `core-restarts.md`.
- *Superseded 2026-10-23: at nominal cert-issuer is spoken to over Cassini, as everything else is, so it hears the
  authority updates as every builder agent does ("Step 1, planned", below).* **Delivery by push over the relationship
  that already exists.** The credentials agent is already a verified mTLS
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
    all) and `cassini`. Cassini is questionable; the architecture document says it is a hard
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

- A mode (the architecture document's flag) that sends the request to the observer's HTTP route, which relays it
  over Cassini; the pod has no certificate yet, so it cannot use Cassini itself. **Superseded in detail by "Scope:
  `polar-init` through the observer" below** (the token is sealed by an ephemeral negotiation, not carried over TLS).
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
  (add a policy engine to it), which is the cluster's owner's to decide.

## Scope: `polar-init` through the observer (2026-10-13, read from the code; nothing built)

What moving the 22 non-bootstrap workloads onto the credentials agent actually takes. Four of the findings below
change the shape of the floor or of the existing issuance flow, so they are decisions, not just work.

### Correction (2026-10-13): an agent holds two certificates with two lifecycles

The sections below were written as if a pod's identity were its client certificates. It is not:

- **An mTLS client certificate**, scoped to the pod instance, rejuvenating about hourly. This is what the floor and
  the bundle below are about.
- **A service-identity (agent escrow) certificate** that survives rejuvenation. Its key is escrowed with the
  credentials agent; on the first deployment it is created, on every later one it is retrieved. It has its own flow
  and its own ledger record (`AgentEscrowIssuanceCompleted`, checked with `has_agent_escrow_certificate`, not by the
  pod-instance renewal rule). Every agent has both. Exceptions, for their own reasons: `ace` (not yet a proper Polar
  service; it holds only the Neo4j client certificate), Neo4j (a server certificate), and Cassini (never a client).
- **Direction: `CryptoActor`, inside `polar-init`, owns the lifecycle of both** and passes the workload *handles*, not
  only certificates. Today only the escrow half does that: the actor retrieves the escrow key first and issues it as
  a fallback, and `polar-init` records the kernel keyring number so the workload adopts the key without ever holding
  it. The mTLS half is still `polar-init` writing PEM files from its own `issue_cert` loop, with the key in a file;
  the work file's "harden the mTLS client key" item is this same step.

What that changes below: the observer client belongs in `polar-crypto` (a message that ensures the pod's client
certificates, beside `EnsureBootstrapped`, which does the escrow identity), not as new code in `polar-init`'s file
loop; the bundle covers the pod-scoped certificates only and the service identity keeps its own flow (`Service` is
not requestable in a bundle), but the actor is what calls both, in one place; and the two flows use different
transports, because the first client certificate cannot be fetched over Cassini (the actor needs that certificate to
connect): the observer's TLS route for the pod-scoped certificates, Cassini for the escrow identity after it. The
ledger consequence is that every agent ends with a client-certificate record and an escrow record, and the retrieve
handlers (which need both) work as built.

### What `polar-init` does today

One run per pod, one service-account token, then one `POST /issue` to cert-issuer per `--cert` argument, writing
`cert.pem`, `key.pem` and `ca.pem` into each directory. Transient trouble (unreachable, a malformed response) is
retried every 5 seconds forever; a rejection is fatal (exit 1). After the certificates it runs the escrow bootstrap
over Cassini with the certificate it just obtained, for an agent that needs encryption only (since 2026-10-22,
`policy-enforcement.md`, "Decisions"). The requests across the rendered manifests:

- 15 workloads ask for a client certificate (`/etc/tls/certs`); 6 of those also ask for a second client certificate
  for Neo4j (`/etc/neo4j-client-tls`): `credentials-agent`, `git-repo-processor`,
  `jira-processor`, `kube-consumer` and `polar-scheduler` (the git scheduler, which was a seventh, is gone). `ace` asks only for the Neo4j one.
- 3 ask for a server certificate with names: Cassini (`cassini-ip-svc.polar.svc.cluster.local`), Neo4j (several), and
  the git server, which asks for a client **and** a server certificate (`polar-git-server.polar.svc.cluster.local`).

### What the credentials agent's issuance flow assumes, which does not fit

1. **One certificate per pod instance.** `IssuanceHandler` refuses a request whenever the pod's
   `(identity, instance_binding)` already has a valid certificate (`AlreadyHasValidCertificate`, 403), and a second
   `IssuanceCompleted` for the same identity marks the earlier record revoked as `Superseded`. That is the
   "a pod cannot renew itself" property, and it is right; but a pod that legitimately needs two certificates would be
   refused on the second and, if it were not, would revoke its own first. The request carries one CSR.
2. **Client certificates only.** The handler always signs `CertType::Client`, with `p:` extras taken from a grant
   (a Kubernetes credential needs no grant). `IssuanceRequest` is `{csr_pem, credential}`: no certificate type, no
   names. Cassini, Neo4j and the git server cannot be issued this way.
3. **The observer is plain HTTP.** `axum::serve` on a `TcpListener`, no TLS. `polar-init` would send the pod's
   service-account token, a bearer credential, in a request body in the clear on the pod network, and receive the
   certificate back. The token has to be protected before this carries it; the decision is an ephemeral negotiation, not TLS and another certificate (below).
4. **The ledger keeps one certificate record per `(identity, instance_binding)`**, which the retrieve handlers read
   for the requester's certificate. Server certificates and second client certificates have nowhere to go.

### Proposed shape (revised 2026-10-13 after the decisions)

- **Several separate requests per pod, on the existing flow. No new request kind.** The consumer's two pod-instance
  rules change just enough: the renewal check and the "superseded" marking are keyed on the certificate **type**
  as well as `(identity, instance_binding)`, so a pod instance may be issued one client certificate and one server
  certificate, and a second request of the same type is still refused. The request gains what a server certificate
  needs and a client request never sets: the type and the names. The second Neo4j client-certificate directory is
  not designed for: it belongs to the builder migration, which is the priority, and what the builder's graph
  component actually reads is to be checked in the code when this is built.
- **The token is protected by an ephemeral negotiation, not by TLS and not by another certificate.** The existing
  `ephemeral_exchange.rs` (single-use X25519, HKDF-SHA256, AES-256-GCM, used today for an agent's first escrow-keypair
  retrieval) is the machinery. The negotiation is between the requesting actor and the **consumer**, which holds the
  escrow key; the observer relays the sealed request and never sees the token. The observer therefore stays a plain
  HTTP relay with no server certificate of its own, and the floor does not change shape (the observer remains a
  client-certificate bootstrap identity only). What remains to read from the code rather than ask: how the
  requester knows it is negotiating with the credentials agent (the published escrow public key is the obvious
  anchor, and how the escrow bootstrap handles it today decides it).
- **Who sends it: `CryptoActor`, in `polar-crypto`, in `polar-init`.** A message beside `EnsureBootstrapped` (which
  sets up the escrow identity) that ensures the pod's client certificate; `polar-init` sends the two messages and
  no longer runs its own `issue_cert` loop. The actor is the owner of both lifecycles and, later, of the handles.
- **Consumer rules:** the credential must be a Kubernetes token (an IdP token is refused on this path); a client
  request takes the grant's `p:` extras as now and no names from the caller; a server request's names must all be in
  the allowlist for this identity; the escrow-identity type (`Service`) is not requestable here.
- **cert-issuer.** The floor's list of names per workload is checked on every path, vouched or not. The reason is that
  cert-issuer is moving onto the builder so that it reasons about policy itself with its own infrastructure; until
  then it is configuration, rendered from the catalog, and signed policy takes it over above that.
- **Catalog, resolver, render, `up.nu`.** Each workload's mode (the bootstrap three keep going to cert-issuer
  directly) is a catalog fact; a resolver rule refuses a non-bootstrap workload that names cert-issuer;
  `up.nu` already applies the credentials pair first and waits on it.

### Phases, each tests first and independently deployable

1. **The consumer. BUILT 2026-10-13** (tests first; six rules each broken on purpose and caught). The request gains type and names; the renewal and superseded rules keyed on type; the
   server-name check; the ledger event and state with replay of an existing ledger; a pure planning function tested
   exhaustively.
2. **The sealed request. BUILT 2026-10-13** (seven rules each broken on purpose and caught; not deployed). The flow is
   an ephemeral session, not TLS: the workload asks for a session (`POST /workload/session` on the observer, relayed as
   `OpenSession`); the consumer makes a fresh X25519 keypair, keeps the secret half for 60 seconds (at most 256 open,
   single use, in memory) and returns the public half; the workload seals its token to it with the existing
   `EphemeralEncryptedPayload::encrypt` and sends the sealed request (`POST /workload/issue`) with the CSR, type and
   names in the clear; the consumer opens the session once and hands the unsealed request to the same
   `IssuanceHandler`, so no issuance rule is duplicated. The observer relays opaque bytes and never holds the token.
   One new topic pair, `credentials-workload-issuance-{requests,replies}`, one actor, one reply router. The old
   `/issue` route is unchanged. **What this does and does not protect:** a token cannot be read by anyone who only
   watches the network. It does not authenticate the credentials agent to the workload, so someone able to alter
   traffic between a pod and the observer could answer the session request with their own key; that is the same
   exposure the cleartext route had, not a new one, and closing it needs a trust anchor the pod can check, which is
   the open question left to settle when the workload side is built (phase 3).
3. **The client side. BUILT 2026-10-13** (eight rules each broken on purpose and caught; not deployed).
   - **`credentials-agent/client`** (`credentials-agent-client`): the observer HTTP client, which was the
     user-cert-fetcher's private one and is now shared (the architecture document called for exactly this crate), with
     `open_session` and `issue_sealed` beside `identity` and `issue`.
   - **`polar-crypto::workload_certificates`**: `request_certificate`, one attempt at one certificate: CSR and key
     generated in process, a session opened, the token sealed to it, the sealed request sent. Which failures are worth
     another attempt is decided there and said in the type (`RequestError::{Retry, Refused, Failed}`): a flooded or
     absent observer, an expired session and a signing failure are retried; a refusal is final.
   - **`polar-init`**: `POLAR_INIT_ISSUER` (`cert-issuer`, the default, or `credentials-agent`) and
     `POLAR_INIT_OBSERVER_URL`; in observer mode each `--cert` goes through `request_certificate` and the bundle is
     written as before. Nothing changes for a workload until its environment says `credentials-agent`.
   - **A correction to what I proposed above:** this is a function beside `CryptoActor` in `polar-crypto`, and not a
     message on the actor. The actor is spawned over a Cassini connection, and that connection needs the very client
     certificate being requested here, so the actor cannot exist yet when the request is made. It is the first half of
     the crypto lifecycle; the actor takes over once there is a certificate to connect with, and is where the keys will
     be held by handle when that work lands. The returned private key is in a `Zeroizing` string and is written to the
     requested directory as before.
   - **Not settled:** how a pod tells it is talking to the credentials agent (see phase 2).
4. **cert-issuer. BUILT 2026-10-13** (tests first; five rules each broken on purpose and caught, and a sixth found
   uncovered and given a test; not deployed). The floor's list now holds every workload that has names written down,
   and the names are checked on **both** paths. `BootstrapIdentity` gains `vouched_only` (default false). A
   token-only request is admitted only for an entry that is not `vouched_only`. A request the credentials agent
   vouches for (`admit_vouched`) may carry the permissions of a grant (`p:`, which are the credentials agent's to
   state) and otherwise only names on the list of the token's workload **for the type it asks for**; a workload with
   no entry may carry no names, and `c:` is still refused. So a credentials agent that is mistaken or subverted cannot
   have a workload signed with a name that is not its own. `/validate` follows the same rule (a `vouched_only`
   workload is not answered on its token alone). The chart's `mtlsBootstrap` gains `vouchedOnly` and two entries,
   Neo4j (`graph` in `polar-graph`, the names its init container asks for) and the git server. **Duplication to
   remove in phase 5:** Neo4j's names are written here and in the target's `neo4jSans` override; the catalog should
   render both from one place. Still off: `mtlsEnforceBootstrap = False`, and absent `mtls.bootstrap` is the old open
   behaviour, so none of this applies on the cluster until the floor is enabled in phase 6.
5. **Catalog and resolver. BUILT 2026-10-13** (tests first; seven rules and wirings each broken on purpose and caught;
   not deployed). Each workload has an `issuer` (`cert-issuer` or `credentials-agent`; the kind's default is
   `cert-issuer`, today's behaviour, and a target moves a workload by setting it, so the rollout goes in groups) and a
   `bootstrap` flag (cassini, the credentials agent and its observer). A workload on the credentials agent is given
   `POLAR_INIT_ISSUER` and `POLAR_INIT_OBSERVER_URL` (the observer is in `services.nuon`) and a **blank**
   cert-issuer address, so nothing in its pod is told where cert-issuer is. The rule (`rules.nu`, `issuer_problems`):
   an unknown issuer, or a bootstrap identity on the credentials agent, is always an error; a non-bootstrap workload
   on cert-issuer is listed as *pending* by `resolve.nu check` while `cluster.nuon`'s `issuance.floor_enforced` is
   false, and an error once it is true, because cert-issuer would then refuse it. **The floor is written from the
   catalog** (`resolve.nu floor`, `floor_entries`): the bootstrap identities with the one type and the names they are
   issued, and a `vouchedOnly` entry for each server certificate of a workload on the credentials agent (the git
   server, when it moves). `render.nu` hands these, and `floor_enforced`, to the cert-issuer chart; the chart's
   `main.nu` adds Neo4j's entry from its chart and the target's `neo4jSans`, so those names are written once. The
   chart's own `mtlsBootstrap` is now empty. Neo4j itself is not yet on the credentials agent: its StatefulSet is a
   chart, not a catalog workload, and moving it is part of phase 6.
6. **Rollout on the cluster, DONE 2026-10-14 on `local-minimal`** (a fresh cluster, so no workload had to be
   moved while running; the order below is the order it was proven in). Each group was one target override, one
   `up.nu` apply, a check of every pod's init log, and the secret harness.
   - **Baseline (everything on cert-issuer):** the harness failed with `RequesterCertificateNotFound`, as the
     analysis predicted.
   - **Group 1, the harness pair.** Found two defects in what the credentials agent records, fixed at the source
     (`docs/architecture/credentials-agent-architecture.md`, "Which identity"): the issuance event and the service
     certificate were recorded under the token's raw identity, not the certificate's. After the fixes, on a clean
     ledger, the harness passes.
   - **Group 2, the five workloads that ask for one client certificate** (the observers and the OCI resolver).
   - **A change to the `graph-write` capability:** a graph agent presents the pod's one client certificate to Neo4j
     (`GRAPH_CLIENT_CERT`, `GRAPH_CLIENT_KEY` and `GRAPH_CA_CERT` point at `/etc/tls/certs`) and no longer asks for a
     second one, which the credentials agent refuses (one client and one server certificate per pod instance). The
     second was the same identity and type with no extra names; Neo4j accepts any client certificate the Polar CA
     signed (`client_auth=REQUIRE`), so it said nothing the first did not.
   - **Group 3, the five graph agents,** and **group 4, the git server** (a client and a server certificate, the first
     server certificate through this path).
   - **Neo4j and the ACE job,** which are charts and not catalog workloads: they are told the observer's address
     (from `services.nuon`, through `resolve.nu service-url` and `render.nu`) and no cert-issuer address
     (`tests/charts.nu`). The comment in Neo4j's chart that this would deadlock was out of date: the consumer
     starts without Neo4j.
   - **The floor is enforced** (`cluster.issuance.floor_enforced`). A canary asks cert-issuer, with no client
     certificate, to validate a real token: before, `kube-consumer` and the bootstrap observer both got 200; after,
     `kube-consumer` gets 403 and the observer 200. Cassini's own issuance path was exercised the same way: its own
     server name is signed, another name or the client type is refused. A pod restarted on each path afterwards
     (a moved workload, the git server's vouched server certificate, a bootstrap identity) started without a refusal.
   - **Not done:** NetworkPolicy emission with the canary (the control below); the hand-written agent charts that
     are not in `local-minimal` (gitlab, openapi, build-processor) still ask cert-issuer directly, so a target that
     deploys them cannot enforce the floor until they move; and the larger gap this exposed, that nothing checks a
     certificate's permissions on service-to-service traffic (`remaining-work.md`, the top item under
     "Next: orchestration and Justfile").

Phases 1 to 5 can be built and tested without touching the cluster. Phase 6 restarts every pod.

### Settled, and what is left

Settled: several separate requests (no bundle); an ephemeral negotiation, not TLS on the observer; the allowlist
lives with cert-issuer because cert-issuer reasons about policy itself; nothing special for agents not yet on the
builder; a name not on the allowlist is refused; the crypto actor owns both certificate lifecycles. Left, to be
read from the code and decided in the build, not asked: how the requester authenticates the credentials agent in the
negotiation, and what the builder's graph component reads for its Neo4j connection.

## Every certificate cert-issuer signs becomes known to the credentials agent (2026-10-23)

**The requirement (the maintainer, 2026-10-23).** cert-issuer never signs a certificate that the credentials agent
does not eventually know of: "Credentials agent is cert-issuer's body guard." It may not know at bootstrap, but the two
should have that conversation as soon as they can. This is the ledger record in "The invariant wanted", above, applied
to the bootstrap path as well. And the floor's purpose (the maintainer, 2026-10-23): "to prevent anyone from talking
to cert-issuer directly, once the core had brought the system to nominal operation." The token-only path exists only to
bring the core up; it is not a standing way in. A target that does not enforce the floor is a gap against this, not an
option.

**The gap, read from the code.** The ledger learns of a certificate only from the credentials agent's own issuance
(`IssuanceCompleted`, `credentials-agent/consume/src/state.rs:331`; `certificate_for`, `state.rs:555`). Nothing tells
it of what cert-issuer signs on a token alone:
- the bootstrap services' certificates: the credentials agent's own, its observer's and Cassini's
  (`infra/config/workloads/credentials.nuon:34`, `:54`; `cassini.nuon:35`), signed anew at each start, and
  cert-issuer's own TLS certificate, which it signs for itself at start (`cert-issuer/server/src/tls.rs:95`);
- on a target that does not enforce the floor (`infra/config/cluster.nuon:55`, `floor_enforced: false` by default;
  `sandbox-local` and `local-minimal` enforce it), every workload whose `polar-init` still asks cert-issuer directly;
- a certificate the credentials agent asked for and never recorded: one it crashed between cert-issuer's answer
  and its ledger append.

What it costs today:
- Cedar at the credentials agent cannot see a bootstrap service's groups, since a check finds the requester's
  certificate in the ledger (`store_secret.rs:113`). The observer publishes six of the thirteen request topics, so
  this bites the request gate of `authority-model.md` slice 9.
- A bootstrap service's certificate cannot be revoked, since the credentials agent revokes from its records.
- The public-certificate lookup cannot answer for one.

**Proposed shape.** Every signing goes through the `CaClient` trait (`cert-issuer/server/src/ca.rs:122`): the
handler's (`handler.rs:387`) and cert-issuer's own TLS certificate (`tls.rs:95`). So the record is written there, by a
`CaClient` that records and then hands out, and no signing can go around it.
- **cert-issuer records each certificate before it hands it out.** It appends an entry to a log on its own durable
  volume, and syncs it, before answering: a sequence number, the serial, the identity, the type, the path it came by
  (token only, vouched, `/internal/issue`), the time, and the certificate itself. A certificate is public, so the log
  holds nothing secret. If the append fails, the certificate is not returned, so none can leave unrecorded. It
  records every path, not only the token-only one: the vouched and internal entries are how the credentials agent
  finds one it asked for and lost.
- **The credentials agent asks over Cassini.** cert-issuer has no dependency on Cassini but may speak over it, and at
  nominal it is spoken to as everything else is ("The model", revised 2026-10-23). So cert-issuer becomes a Cassini
  client once Cassini is up, and the record is asked for and answered on a request and reply topic pair. Only the
  credentials agent may ask; the broker verifies who asks (`published_by`), and cert-issuer checks it. The credentials
  agent asks with the last sequence number it holds, and cert-issuer answers with every entry after it. Both start
  after Cassini at bootstrap too, so the first ask needs no other channel: the credentials agent asks when it starts,
  and again on a short interval, since a bootstrap service's certificate lives an hour (`values.dhall:53`, `:54`).
  cert-issuer's signing, and its health, never wait on that client: a record not yet asked for waits in the log.
- **The ledger holds a reported certificate as it holds one it issued.** A new event, `CertificateReported`, carries
  the entry and cert-issuer's sequence number, which is also the cursor the credentials agent resumes from. An entry
  whose serial the ledger already holds adds nothing. Otherwise the certificate is a `CertificateRecord`, marked as
  reported by cert-issuer: `certificate_for` finds it, a newer one supersedes it, and it can be revoked. A token-only
  entry for an identity the model does not call a bootstrap service is recorded, and reported as unexpected (a
  warning, and in the graph), since on an enforcing target it should not exist.
- **Retention.** An entry may be dropped from cert-issuer's log once the credentials agent has asked past it and the
  certificate has expired. The ledger keeps everything.
- **Storage.** The CA's volume is a writable PVC only in `SelfBootstrap` mode. In `Provisioned` mode it is a read-only
  Secret (`infra/layers/2-services/cert-issuer/deployment.dhall`). The log gets its own small PVC in both modes, apart
  from the CA's key.

**cert-issuer joins the builder (2026-10-23).** Its Cassini client comes from moving it onto `polar-agent-builder`,
where Cassini (`cassini/broker/src/agent.rs`) and both halves of the credentials agent already are: the topic gate on its
own traffic, `published_by` on what it hears, a scaffold (so the catalog and the authority model know its topics), and
the builder's health check, signals and shutdown. Read from the code, one thing must change first: an agent's health
counts its Cassini session (`polar-health/src/machine.rs:207`, unhealthy on a disconnect, failed by the tick if the
outage lasts), and the builder's optional Cassini (`PolarAgent::OPTIONAL`, `polar-agent-builder/src/agent.rs:112`) is
chosen once per deployment. A Cassini restart would then restart cert-issuer: one core service restarting would become
two (`core-restarts.md`, situation 4). So the builder gains a Cassini client that does not count toward health, as
`expects_graph` decides for the graph (`machine.rs:221`), for a service with no dependency on Cassini. cert-issuer signs
its own client certificate with its CA at start, as it does its TLS certificate, and the record holds it.

**The order (cleared 2026-10-23).** 1, cert-issuer gets a Cassini client by joining the builder; 2, the certificate
record, asked for over Cassini; 3, `authority-model.md` slice 9, whose request gate then finds every requester's
certificate in the ledger; 4, the questions of `core-restarts.md`, one at a time.

**Step 1, planned (2026-10-23), each part tests first.** Read from the code: cert-issuer is a plain `tokio::main`
(`cert-issuer/server/src/main.rs`) reading a JSON file named by `CERT_ISSUER_CONFIG`, with no health check, signal
handling or shutdown handshake, run by a hand-written Deployment (`infra/layers/2-services/cert-issuer`), and not in the
catalog. Cassini made the same move (`cassini/broker/src/agent.rs`), and so did the credentials pair, first onto the
builder and then into the catalog (`configuration.md`, 9.8), so the precedent is theirs.
- **1a, the builder: a Cassini client that does not count toward health.** `polar-health`'s machine gains for Cassini
  what `expects_graph` is for the graph: a lost session, or one never made, neither marks the agent unhealthy nor fails
  it at the deadline (`machine.rs:207`, `check_cassini_deadline`). `PolarAgent` says so for an agent; every agent that
  says nothing keeps today's behaviour. The client still connects, reconnects and passes the topic gate. Tests: a
  disconnect and an outage past the deadline leave such an agent healthy and fail any other; it reconnects; a property
  that the health verdict of an agent that counts Cassini is unchanged for any sequence of events.
  *Built 2026-10-23:* `PolarAgent::DEPENDS_ON_CASSINI` (default on; `run` sets the health's `expects_cassini` from
  it), `HealthCheckArgs::expecting_cassini`, `HealthConfig::expects_cassini`, and the machine leaving the session out
  of its judgement when it is off (`connections_healthy`, the disconnect, `check_cassini_deadline`), while its file
  still reports the session. Tests: the disconnect writes nothing; healthy past the deadline, never connected or lost,
  where an agent that expects Cassini gets the deadline's verdict; it still rejuvenates; the real actor gives no
  verdict either way; the setting reaches the health from the trait, and an agent that says nothing counts its
  session. The property is stated as an equivalence: an agent that does not expect Cassini is judged as one that
  expects it and always has it, for any sequence of events.
- **1b, cert-issuer as a `PolarAgent`.** Its settings declared in a `polar_env` section, as Cassini's are, with the
  issuers and the floor as structured values, as the credentials agent's authority model is; its scaffold; its HTTP
  server run inside the builder's lifecycle (signals, shutdown budget). Its client certificate is signed by its own CA
  at start, as its TLS certificate is (`tls.rs:95`). It has no topics of its own yet (step 2 adds the record's); the
  topics every agent has come with the client, the authority updates among them, so it hears the policy in force,
  which is how "it reasons about policy itself" (above) is delivered: over Cassini, not by a push over mTLS. The hand
  chart gives it the builder's environment until 1c. Tests: what each of today's main.rs steps does (the CA loaded or
  made, the TLS certificate, the floor and its warning, the routes served), now through `start`; it serves with no
  Cassini at all; `polar_agent_tests!`.
- **1c, cert-issuer in the catalog.** A workload entry, `kind: "service"`, `bootstrap: true`, its CA volume and its
  floor as the render writes them today, in place of the hand chart, as Cassini's and the credentials pair's entries
  replaced theirs. Tests: the render's, `infra/tests`, and the chart's checks of the floor carried over.

Settled 2026-10-23: cert-issuer rejuvenates, as every agent does (the builder ends an agent when its shortest-lived
certificate nears expiry, `polar-health/src/machine.rs:373`). The core plans its rejuvenation together so that no two
of its members go at once (`core-restarts.md`); until that plan is built, each rejuvenates on its own schedule, as
Cassini and the credentials agent do today.

Certificates signed before this is deployed are not in the log. A bootstrap service's is replaced within the hour,
and every later one is recorded.

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

## Decisions (2026-10-13)

- **Bootstrap set, for now:** `credentials-agent`, `credentials-agent-observer`, `cassini`. Tightening it is a
  follow-up.
- **The dev cluster runs on the no-enforcement fallback** for now; adding a policy engine to it is a
  follow-up.
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
5. ~~The dev cluster's enforcement~~ decided above: the fallback now, a policy engine later.

## What I did not check

- Whether finding 2 is exploitable end to end (no test was run, and no certificate was requested).
- What Cassini does with the `p:` SANs it sees, and what Neo4j does with them (whether either grants access by them).
- Whether any workload depends on being able to choose its own extras (the 22 client certificates ask for none; the
  three server certificates ask for names, listed above).
- The CNI options for the dev cluster beyond naming them.
