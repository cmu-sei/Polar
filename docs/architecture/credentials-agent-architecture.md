# Credentials Agent — Architecture

## Purpose

The Credentials Agent is the clusters single trusted broker for credential
distribution. Every Polar participant — human or agent — that needs a
credential for a legitimate purpose asks the Credentials Agent for it,
rather than that credential being scattered across ad hoc config, baked
into images, or exchanged directly between the two parties who need it.

Human certificate issuance (this documents primary worked example) is
one instance of that charter, not the whole of it. The same agent is
equally the answer to:

- An agent needing another agents public key, to encrypt a payload
  before publishing it to a topic.
- An agent needing a published, known credential to reach an external
  resource (a repository access token, for example).
- An admin granting or revoking a humans certificate permissions.
- Any future participant that needs to ask "what credential do I use
  for Y, and am I allowed to have it."

This follows the same shape as every other durable, cross-cutting piece
of state in Polar: rather than each agent inventing its own answer,
one agent owns the canonical answer and everyone else asks it.

## Why payload-level credential distribution, given mTLS already exists

Every Cassini connection is mTLS. That protects data in transit between
a participant and the broker. It does not protect a payload once its
anywhere else in the pipeline — and there are real such places. Cassini
clients maintain a local, disk-backed retry queue (`QueueEntry::append`/
`drain`, in `cassini-client`) for messages that could not be delivered
immediately. Anything written to that queue in plaintext is exposed at
rest, on disk, regardless of how well the connection that originally
carried it was protected. The broker itself, and any future logging or
debugging tooling built against it, is a similar potential exposure
point — Cassini does not inspect payloads today, but nothing prevents an
operator error, a future feature, or a compromised node from doing so.

For data whose sensitivity outlives the transport hop — an encrypted
payload meant for one specific recipient, not "anyone who can currently
read this connection" — payload-level encryption is a second, independent
layer of protection. The Credentials Agent is where a participant gets
the public key it needs to apply that layer.

## Architecture: observer and consumer

Like every other Polar agent, the Credentials Agent has two halves with
deliberately different trust levels.

**Consumer (high-trust)**: owns the canonical, durable state. Subscribes
to grant, revocation, and issuance events on Cassini and writes the
authoritative record to the graph. Answers authenticated, high-trust
questions — "what is currently granted to this identity" — only to
callers who have already established their own trust (an mTLS-connected
Polar participant), the same bar every other consumer-side interaction
in this system already requires.

**Observer (low-trust)**: the only part of this whole system that a
humans own tooling ever talks to directly. Accepts external input —
a users CSR-plus-token bundle, an external verifiers revocation-status
query — and does the absolute minimum: basic well-formedness checking,
then forwards. It never validates a token, never makes an authorization
decision, never touches signing key material. That decision-making stays
entirely on the high-trust side. This mirrors every other observer in
Polar, whose job is to safely receive from an untrusted world without
itself being trusted to decide anything — the only difference here is
that this observers untrusted world is a human at a keyboard rather
than an external system like GitLab or Jira.

Critically, this means `cert-issuer` — the one component that holds the
CAs live signing key — is never directly reachable by user-facing
requests at all. A humans tool talks to the Credentials Agents
observer; everything else happens on the trusted side of the broker.

## Two independent lifecycles

### Admin bootstrap (one-time, per admin)

An admin proves themselves once, using a credential that already exists
independently of anything Polar-specific — for the bootstrap case, a
`kubectl`-admin-backed Kubernetes credential, validated by `cert-issuer`
via the exact same trust path already used for workload certs. In
exchange, they receive an ordinary Polar client certificate — nothing
more privileged than any other agent gets. That cert is what lets their
own tooling connect to Cassini as a genuine, mTLS-authenticated
participant afterward.

This step exists because someone has to be first, with nothing else yet
able to vouch for them. It is not part of the steady-state flow below,
and does not recur unless a new admin needs to be established.

### Admin grants and revocations (repeatable, ordinary agent traffic)

Once bootstrapped, an admins ongoing actions are unremarkable Cassini
publishes, using their normal client cert:

- **Grant**: `{ target_identity, permissions, granted_by, timestamp }`,
  published to a credentials-grants topic. `target_identity` must be the
  exact claim value the relevant `IssuerConfig.workload_identity_claim`
  will later extract from that users real token — the admin confirms
  this by querying the IdP directly for the accounts current value
  before publishing, rather than guessing or reusing a stale value.
- **Revoke**: `{ serial, reason, revoked_by, timestamp }`, published to a
  credentials-revocations topic, for either a human-issued or
  workload-issued certificate — revocation is uniform across both, since
  both are ultimately just certificates with serial numbers the
  Credentials Agents consumer half has recorded.

The Credentials Agents consumer writes both as durable graph state.

### User self-service issuance (steady state, fully automated)

This is the flow that actually issues a certificate to a human, and it
requires no admin presence at the time it happens — only a prior grant,
made independently, at any earlier point.

1. The user authenticates against the IdP directly (Authorization Code
   flow, unchanged from the original design) and receives a real access
   token.
2. Their tool generates a keypair locally — the private key never
   leaves the machine — and builds a CSR. The CSR embeds a hash of the
   raw token as a CSR attribute, cryptographically binding the two
   artifacts together: a CSR generated in possession of one token cannot
   later be silently paired with a different one.
3. The tool sends the CSR-plus-token bundle to the Credentials Agents
   **observer** — not to `cert-issuer`, and not to the consumer side.
4. The observer forwards it, unexamined beyond basic well-formedness,
   onto the trusted side via Cassini.
5. On the trusted side: the token is validated for real (the existing,
   already-tested multi-issuer `oidc.rs` logic — signature, issuer,
   audience, expiry) and, since local signature validation alone cannot
   detect a session that was revoked or logged out server-side before
   natural expiry, corroborated with a live call to the IdPs token
   introspection endpoint (RFC 7662). The Credentials Agent looks up
   whatever grant exists for the now-validated identity, and checks that
   no currently-valid certificate already exists for this exact
   `(identity, instance_binding)` pair (see "Pods cannot request their
   own renewal," below). `cert-issuer` combines identity (from the
   token, used unmodified) with permissions (from the grant) into the
   certificates SAN set, and signs with its own key — no delegated
   signing authority is needed for this path, since the admins earlier
   grant is what authorizes the request, not the admins presence at
   request time.
6. The signed certificate is published back through Cassini, through
   the Credentials Agent, and back to the users tool — the same
   asynchronous path in reverse.
7. The issuance itself is published as an event, the same shape whether
   it came from this path or from a workloads `/issue` call, giving the
   Credentials Agents consumer complete visibility into every
   certificate that has ever been issued, and therefore a complete,
   authoritative revocation list.

## Request/reply over an inherently pub/sub transport

Several of the interactions above — the users tool waiting for a signed
certificate, `cert-issuer` waiting on a grant lookup — need to feel like
a synchronous call, even though Cassinis actual client protocol
(confirmed directly against `cassini-client`s source) offers no native
request/reply primitive, only `send_message`.

This is solved the same way any request/reply pattern is built on top of
a pub/sub-only transport: by convention, not new broker capability. A
requester generates a correlation ID, publishes a request carrying it,
and either subscribes to a response topic or relies on the brokers
existing per-session delivery (`registration_id`-addressed `PushMessage`
delivery, already real and working in Cassini today) to receive a reply
tagged with the same ID. The responder does the matching publish. No
change to Cassini itself is required — this is a client-side
convention, and one specific enough that it is worth formalizing as its
own small, precise spec so any future agent needing the same shape of
interaction can reuse it rather than reinvent it.

## Core services vs. everything else

Two distinct populations exist for `polar-init`s purposes, driven by a
new flag (not a new binary — `polar-init` stays entirely CLI/env-driven,
consistent with its existing design) pointing it either at
`cert-issuer` directly or at the Credentials Agents observer.

**Core services — Cassini and the Credentials Agent themselves —
continue talking to `cert-issuer` directly.** This is not a convenience
choice; it is structurally required. The observer-relay path *is*
Cassini being up and the Credentials Agent being a working participant
on it. Cassini needs its own cert before any broker exists to relay a
request through; the Credentials Agent needs its own cert before it can
act as a Cassini participant at all — bootstrapping through its own
observer would be circular. Every other services direct access today
is incidental, not load-bearing, and can move to the relay path without
issue.

**Everything else talks only to the Credentials Agents observer.**
The observer does not need to distinguish a humans IdP token from a
workloads Kubernetes token to do this correctly — it forwards either
kind blindly, exactly matching its "accept minimal input, decide
nothing" role, and the trusted side validates either using the same
multi-issuer `oidc.rs` machinery already built and tested for the human
case. One mechanism, both flows.

This narrows direct `cert-issuer` reachability to the smallest possible
set — the two services for whom it is a hard bootstrap precondition,
not a judgment call about what else "feels core." It also introduces a
real, deliberately accepted availability coupling: if the Credentials
Agent or Cassini is degraded, no ancillary service can renew a
certificate either, even with `cert-issuer` itself perfectly healthy.
This is an extension of an already-accepted blast radius, not a new
category of risk — a Cassini restart already cascades new certs and
forced restarts across the whole fleet today, observed directly and
repeatedly during this projects own development.

## Pods cannot request their own renewal

A deliberate, security-relevant property, not an incidental one: a
running pod has no path to extend its own useful lifetime by obtaining
a fresh certificate without dying first. This is what gives forced pod
rejuvenation its actual teeth as a defensive mechanism — a compromised
agent has a fixed, non-negotiable ceiling on how long it has before the
container goes out of scope, and nothing running inside that pod can
push that ceiling back.

Today, this property holds only because the main agent binary happens
not to contain renewal code — an omission, not an enforced rule.
Nothing prevents it from being added later by accident, or by someone
who does not know why it matters. The fix is to make this the
**observers** responsibility, not a convention: the observer checks
incoming requests against the Credentials Agents own issuance
records, keyed on `(identity, instance_binding)` together, and refuses
any request where that exact pair already holds a currently valid,
unrevoked certificate.

`instance_binding` is `IssuerConfig.instance_binding_claim`, present
since v1 but previously unused beyond audit telemetry (for Kubernetes,
the requesting pods own UID). Scoping the check to the pair, not
identity alone, is what makes this safe rather than disruptive:

- **Rolling restart**: an old pod and a new pod briefly coexist during
  a normal Deployment update, both under the same ServiceAccount and
  therefore the same identity. Different UID, since it is a genuinely
  new pod — the new pods first request is correctly allowed even
  while the old pods cert is still valid.
- **Health-check-triggered rejuvenation**: `restartPolicy: Never` plus
  the Job controller mean a `polar_health` deadline firing does not
  restart a container in place — it fails the pod outright, and the
  Job controller schedules an entirely new one. New pod, new UID, the
  same mechanism as a rolling restart, with no special-casing required.
- **A pod requesting its own renewal**: same identity, same UID, cert
  already valid — correctly refused. This is the actual defensive
  case the rule exists for.

## What is deferred

- OCSP, as a live alternative or complement to a periodically-published
  CRL. The CRL is the simpler mechanism to get working first; OCSP is a
  documented, real future upgrade, not a rejected idea.
- ~~The correlation-ID request/reply patterns own formal specification~~
  — done: see `docs/architecture/cassini-request-reply-protocol.md`.
  referenced here, written up separately.
- Anything beyond the human-certificate use case for general credential
  distribution (agent-to-agent public key exchange, published external
  credentials) — the charter is stated here; the concrete mechanics are
  future work.
