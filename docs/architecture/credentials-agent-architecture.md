# Credentials Agent — Architecture

## Purpose

The Credentials Agent is the cluster's single trusted broker for credential
distribution. Every Polar participant — human or agent — that needs a
credential for a legitimate purpose asks the Credentials Agent for it,
rather than that credential being scattered across ad hoc config, baked
into images, or exchanged directly between the two parties who need it.

Human certificate issuance (this document's primary worked example) is
one instance of that charter, not the whole of it. The same agent is
equally the answer to:

- An agent needing another agent's public key, to encrypt a payload
  before publishing it to a topic.
- An agent needing a published, known credential to reach an external
  resource (a repository access token, for example).
- An admin granting or revoking a human's certificate permissions.
- Any future participant that needs to ask "what credential do I use
  for Y, and am I allowed to have it."

This follows the same shape as every other durable, cross-cutting piece
of state in Polar: rather than each agent inventing its own answer,
one agent owns the canonical answer and everyone else asks it.

## Why payload-level credential distribution, given mTLS already exists

Every Cassini connection is mTLS. That protects data in transit between
a participant and the broker. It does not protect a payload once it's
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

## Source of truth: an event-sourced log, not the graph

The Credentials Agent's actual record of truth is a local, append-only,
hash-chained JSONL log — not Neo4j. Every grant, revocation, issuance
request, and issuance outcome is written to this log in full: timestamped,
containing the complete request body and (once known) its response, and
a hash that incorporates the previous entry's hash. Tampering with any
historical entry breaks every hash after it — the real property a
blockchain would provide, obtained here without any of a blockchain's
actual machinery (distributed consensus among mutually untrusted
writers), which doesn't apply: there is exactly one trusted writer, the
consumer itself.

Only the consumer (the high-trust half) ever touches this log. It is
never a distributed store and never intended to be — a single,
`PersistentVolume`-backed file, mirroring how `cert-issuer`'s own CA
material is already persisted today.

**On startup, the consumer replays this log in full to rebuild its
current state from scratch**, rather than trusting whatever Neo4j
currently contains. This is the actual point of event sourcing here:
Neo4j can be wrong, stale, or directly mutated out-of-band, and none of
that can corrupt the real record, because the real record isn't in
Neo4j at all.

**Neo4j's role is reduced to a disposable, rebuildable projection** —
the public surface needed for observability, and nothing more. If it's
ever lost or corrupted, it's reconstructed by replaying the log again;
it holds no information the log itself doesn't already contain in full.

Durability: every write is `fsync`'d before the consumer acknowledges
it. Issuance/grant/revocation events are low-frequency, not a hot path,
so this costs nothing meaningful in practice, and an audit gap on a
crash immediately after signing a certificate is not an acceptable
tradeoff for the write-latency it would save.

Growth: the log is allowed to simply grow, unbounded, for now. Replay
time at startup grows with the log's entire lifetime history, and a
future checkpoint/compaction mechanism is a real, known option if that
ever becomes an actual problem — deliberately not built pre-emptively.

**Grants are single-use.** A grant is consumed the moment it
successfully authorizes an issuance — not by mutating or deleting the
original grant event (an event-sourced log doesn't rewrite history),
but as a fact derived from replay: a `Grant` event followed later by a
successful `IssuanceCompleted` event referencing it means current state
says *consumed*. A user needing a certificate again after that requires
a fresh grant from an admin.

**A reissue for the same identity supersedes any prior, still-valid
certificate for it.** `IssuanceCompleted` now also carries the actual
signed `certificate_pem` (see "Secret escrow" and the
certificate-lookup query, below, for why -- an agent asking for
another's current public cert needs a real answer). When a new
`IssuanceCompleted` event is folded, any prior, non-revoked certificate
for the same identity -- regardless of `instance_binding`, since a pod
restart gets a fresh one -- is marked revoked in place, with a
synthetic `Superseded` reason, not a real `Revoke` event. This is what
makes the certificate-lookup query well-defined: at most one
non-revoked certificate can exist per identity at any time, so a
lookup never has to guess between candidates or apply an arbitrary
tie-break.

## Cryptographic material: two separate keypairs

The Credentials Agent holds two genuinely distinct keypairs, never one
reused across both roles — reusing a single asymmetric keypair across
two different cryptographic protocols (TLS authentication and
general-purpose encryption) is a real smell in cryptographic
engineering, even when nothing obviously breaks from doing so:

- **Its own Polar mTLS client certificate**, obtained via the same
  `cert-issuer` path every other agent uses — this is what lets it
  connect to Cassini as a genuine, authenticated participant.
- **A dedicated escrow keypair**, used only for the secret-escrow
  mechanism below. Its public half is what other agents encrypt secrets
  to; its private half never leaves the consumer.

## Secret escrow

The Credentials Agent's broader charter (an agent needing a published
credential to reach an external resource — a repository access token,
for example) means real, long-lived secret material can flow through
this system, not just the human-issuance case's public artifacts (a
CSR carries only a public key; a signed certificate is meant to be
presented). If every event were logged with a plaintext payload, a
future secret-distribution use case would put that secret in plaintext,
permanently, in a file whose entire purpose is being replayed and read.

The escrow model, worked out end to end:

1. A participant holding a secret to hand off (Jira Agent holding a
   Git token, for example) encrypts it directly to the Credentials
   Agent's escrow public key *before* sending — using a type-safe Rust
   construct, `EscrowedSecret`, wrapping ciphertext plus which key it's
   encrypted to. The only way to construct one from plaintext is
   through the function that actually performs the encryption; there
   is no ergonomic, accidental path from a plain `String` sitting in
   memory to an `EscrowedSecret` that skips encryption, the same "make
   the unsafe state unrepresentable" principle already used for
   `DynamicNodeKey`/`BackendJobIdentity` in the graph library.
   (Unlike those two types, `EscrowedSecret` genuinely has to cross a
   process boundary -- deserializing one from wire bytes on receipt is
   a distinct, necessary operation, reconstructing a value that was
   already, provably produced by the encryption function in a
   different process, not a second path to fabricate one from
   plaintext.)
2. What arrives at the Credentials Agent, and what the event log ever
   contains, is always this one stable ciphertext form — encrypted to
   the Credentials Agent's own escrow key, regardless of who eventually
   requests it or how many times, or how far in the future.
3. The Credentials Agent can always decrypt what it holds (it's its own
   key) but has no reason to at rest — only transiently, at the exact
   moment a specific, authorized consumer actually needs the secret:
   decrypt with its own private key, immediately re-encrypt with that
   consumer's public key, send, and destroy every intermediate
   plaintext copy.
4. That transient plaintext window is the one place real secret
   material exists in memory at all, and it's treated as a hard
   requirement, not "be careful": decrypted bytes are handled via
   `zeroize`/`secrecy`-style wrapping, explicitly wiped the instant
   re-encryption completes, rather than relying on Rust's ordinary
   `Drop` (which guarantees deallocation, not zeroing beforehand).

This is not homomorphic encryption — the Credentials Agent genuinely
holds plaintext for a brief window on every relay. Short of adopting
homomorphic encryption (a much larger, currently unwarranted undertaking),
this is the strongest practical property available: no secret is ever
at rest anywhere in this system, including this system's own audit log,
except encrypted to a specific holder's own key. This is deliberately
not a zero-knowledge design -- the Credentials Agent is a trusted
custodian *with* access, not one deliberately locked out of what it
holds. See "Agent escrow bootstrap," below, for the one place this
general model needs a real, narrow exception (retrieving one's own
escrow keypair for the very first time, when no stable key yet exists
to receive the response encrypted to).

**Encryption scheme**: a standard hybrid cryptosystem, using only
FIPS-140-approved primitives given this project's own compliance
posture (the same reason `rustls` is used in FIPS mode elsewhere in
this system) -- RSA-OAEP (SHA-256) wraps a randomly-generated AES-256
key; AES-256-GCM encrypts the actual secret payload. RSA-OAEP alone is
not used directly on the payload: it has a hard plaintext-size ceiling
(roughly 440 bytes at 4096-bit/SHA-256-OAEP) that an arbitrary secret
should not be assumed to fit under.

## Agent escrow identity: pod-instance vs. service identity

Every other agent that wants to *participate* in secret escrow (not
just the Credentials Agent itself) needs its own dedicated escrow
keypair, mirroring `EscrowKeypair`'s own pattern — used only to
encrypt payloads to, and decrypt payloads from, other Polar agents.
Never the agent's mTLS client certificate, for the same reason the
Credentials Agent's own two keypairs are kept separate: reusing one
asymmetric keypair across two different cryptographic protocols is a
real smell, even when nothing obviously breaks.

**Two distinct layers of identity are in play, not one:**

- **Pod-instance identity** (the existing mTLS client certificate):
  proves "I am a currently-live, running process embodying this
  service, right now." Genuinely ephemeral — a new keypair every
  single rejuvenation, even though the *name* it asserts (the
  FQDN/SAN, tied to workload identity) is stable across restarts.
  `instance_binding` is the separate claim that actually changes each
  time. This is also now the mechanism `published_by` on
  `ClientEvent::MessagePublished` is grounded in -- extracted directly
  from the connecting client's verified mTLS certificate at the
  broker level, never self-reported, which is what makes
  identity-based authorization below possible without a separate
  signature or trust decision.
- **Service identity** (the new escrow keypair): the durable,
  cryptographic identity of the *service itself*, persisting across
  however many pod instances have ever embodied it. Not tied to any
  one process's lifetime.

**Root of trust**: an escrow keypair is not self-asserted or
established merely by publishing it over a broker-verified connection.
It goes through a real CSR flow against `cert-issuer` — the same
underlying machinery already hardened for human/workload mTLS
issuance, reused for its proven pattern, not literally reissued as an
mTLS cert (wrong purpose: this key is for encryption, not TLS
handshakes). The agent generates its keypair locally, builds a CSR,
and receives back a CA-signed escrow certificate, anchored to the same
Smallstep CA root every other certificate in this system already
trusts. Any agent encrypting to another agent can independently verify
the recipient's escrow certificate against the root CA directly — not
merely trust the Credentials Agent's own bookkeeping that a given key
belongs to a given identity.

**Issuance is automated, not admin-mediated — for now.** Unlike human
certificate issuance, which requires a real admin's prior `Grant`
before a user's tool can obtain a certificate, an agent's escrow
certificate is issued automatically: the agent's own, already-validated
service identity (the same Kubernetes-credential validation path admin
bootstrap already uses) is sufficient authorization on its own, with
no separate grant lookup — mirroring admin bootstrap's own reasoning
that the underlying credential already authorizes the exchange.

But unlike admin bootstrap's own resulting certificate ("nothing more
privileged than any other agent gets"), an escrow certificate is not
permission-less. The Credentials Agent embeds a permission set into
the certificate's SAN, the same mechanism the human grant flow already
uses — playing the "admin" role itself for this specific flow,
automatically, rather than waiting on a real admin's separate action.
Today, every agent receives the same, uniform permission set; there is
no per-agent differentiation yet. This is a deliberate, named
simplification, not the design ceiling: the intended direction is for
the Credentials Agent to source agent-specific permission configuration
from the scheduler agent (see the scheduler's own credential-schedule
work, a separate, related effort), at which point this flow's
permission set stops being uniform and becomes genuinely agent-specific
— without requiring any change to the issuance mechanism itself, only
to where its permission input comes from.

**Escrowed across rejuvenation, not regenerated.** An agent's own
escrow private key is itself just another secret, escrowed through the
same general `EscrowedSecret` store/retrieve mechanism this document
already describes — no separate, new key-custody system. A first-ever
pod for a given service generates its escrow keypair, gets it
CA-signed, and escrows the resulting keypair (private key included) to
the Credentials Agent for safekeeping. Every rejuvenation after that
retrieves the previously-escrowed keypair instead of generating a new
one — so ciphertext already sent to that service's public key never
becomes orphaned by a routine restart.

**Authorization for retrieval cross-checks both identity layers**: the
Credentials Agent validates that the requester's current,
broker-verified pod-instance identity (`published_by`, extracted from
the live mTLS cert) matches the service-level identity the requested
escrow keypair is registered under. A currently-live pod proving it's
genuinely "the Jira Agent" right now is what authorizes retrieving
"the Jira Agent"'s own, persistent escrow identity — not proof of
possessing any specific prior keypair, which is exactly the property
that makes surviving rejuvenation possible at all.

## Agent escrow bootstrap: retrieve, fall back to issue-then-escrow

**The actual startup sequence, per agent, per pod.** An agent's own
escrow identity bootstrap is retrieve-first, issuance-as-fallback —
not two independent paths. On startup: attempt to retrieve a
previously-escrowed keypair for this service identity. If one exists,
decrypt and proceed (see the ephemeral-exchange requirement below). If
none exists — genuinely the first-ever pod for this service, or store
never having happened for another reason — fall into issuance:
generate a keypair locally, submit a CSR, receive a CA-signed escrow
certificate back, then immediately encrypt the new private key to the
Credentials Agent's own escrow key and store it via the general store
mechanism, so every subsequent rejuvenation retrieves instead of
reissuing.

**Why the normal re-encrypt-to-the-requester's-key pattern breaks
specifically here.** The general secret-retrieve flow (once fully
built) re-encrypts a stored secret to the requester's own,
already-stable, CA-signed escrow certificate — no circularity, since
the requester already has a durable key to receive with. Retrieving
*one's own escrow keypair for the first time* is different: the thing
being retrieved *is* the agent's only durable key, so at the moment of
the request, no such stable key yet exists to encrypt the response to.
The only alternative would be the agent's ephemeral mTLS certificate —
precisely the cross-purpose key reuse this document already rejects
elsewhere. This is a real, narrow circularity specific to this one
bootstrap case, not a flaw in the general store/retrieve design.

**Resolution: ephemeral key exchange for this one response.** The
requesting agent generates a throwaway X25519 keypair solely for this
request, sending its ephemeral public half alongside the retrieve
request. The Credentials Agent generates its own matching ephemeral
keypair, performs an X25519 ECDH exchange to derive a one-time shared
secret (via HKDF), uses it to AES-256-GCM-encrypt the response, and
returns its own ephemeral public key alongside the ciphertext so the
requester can independently derive the same shared secret. Both
ephemeral keypairs are discarded immediately after this single
exchange completes.

**Protecting the escrowed private key after retrieval: the actual
threat this addresses.** The mTLS private key already has a hard upper
bound on any compromise's value — an hour, then it's dead regardless
of what happens to the process. The escrow private key has no such
bound; it's specifically designed to outlive any single pod's
lifetime, which makes it a genuinely more valuable target for a
process-level compromise (an observer parsing untrusted external
payloads, for instance) to steal and carry forward. This doesn't
defend against a still-live, compromised process *misusing* the key
while it runs — that's what runtime attestation (a separate, already-
named piece of this system's own architecture, not yet built) exists
to eventually help operators detect. What this defends against
specifically is key *extraction*: a compromise that reads the raw
private key out of process memory and carries it elsewhere.

**Primary mechanism: the Linux kernel keyring, when the init container
can set it up.** The init container performs the retrieve-or-issue-
then-escrow sequence above and, if the pod's own container-runtime
configuration supports it, adds the resulting private key to the
pod's shared session-keyring via the kernel's asymmetric-key subsystem
(`add_key()`), writing only the resulting integer key-serial handle to
the same shared `emptyDir` volume already used for `cert.pem`/
`key.pem` (a new file, `escrow-key-handle`; its path is `POLAR_ESCROW_KEY_HANDLE`,
built 2026-10-07: `polar-init` writes it from what `EnsureBootstrapped` reports,
and `CryptoActor` adopts the key by it only if the key's description is its own
identity, since the user keyring is shared by UID). `CryptoActor` (below)
references that serial directly for `KEYCTL_PKEY_DECRYPT`/
`KEYCTL_PKEY_SIGN` operations — the raw private key bytes are never
copied into the agent's own userspace-readable memory at all, at any
point.

**Runtime capability detection, not static environment detection.**
Whether a pod's containers genuinely share a kernel keyring namespace
depends on the container runtime's own configuration (gVisor, Kata
Containers, or explicit per-container user-namespace isolation all
break this) — a property that's fragile to detect statically from
inside a container. Instead, `CryptoActor` empirically tries a cheap,
harmless keyring operation (a `KEYCTL_PKEY_QUERY` or equivalent read)
against the handle immediately on startup. Success means kernel-
keyring sharing genuinely works in this deployment, and `CryptoActor`
proceeds using `keyctl` operations directly, never holding the raw
key itself. Failure — or the handle never having been written at all,
because the init container's own attempt to set one up failed or
wasn't attempted — means `CryptoActor` transparently runs the entire
retrieve-or-issue-then-escrow sequence itself, holding the resulting
private key in its own process memory for the pod's remaining
lifetime. Either way, every other actor in the process sees the same,
identical interface from `CryptoActor` — which path was taken is
never visible outside it.

## CryptoActor: the process-local gateway to the Credentials Agent

**The single, exclusive path.** No actor other than `CryptoActor`
itself ever talks to the Credentials Agent, or holds a Cassini
connection for that purpose. Every credential request and every
cryptographic operation any actor in the process needs is a local,
in-process actor call to `CryptoActor` — encapsulating both agent-
escrow-identity custody (see "Agent escrow bootstrap," above) and
general secret store/retrieve behind one, consistent interface.

**Two genuinely different categories of thing flow through it, with
different sharing rules:**

- **The agent's own escrow private key never leaves `CryptoActor`.**
  No operation hands it back to a requesting actor as raw bytes; it's
  used internally, purely to perform sign/encrypt/decrypt operations
  *on behalf of* whichever actor asked. This is what keeps the
  kernel-keyring protection (or, in its absence, `CryptoActor`'s own
  in-memory custody) meaningful — the boundary that actually matters
  is "inside vs. outside `CryptoActor`," not merely "inside vs.
  outside the process."
- **General stored secrets** (a GitLab access token, for example) are
  handed back to the requesting actor as plaintext — there's no
  meaningful "operation" to perform on a bearer token other than using
  it directly. These are a pass-through of whatever a `StoreSecretReply`/
  future retrieve-reply payload from the Credentials Agent itself
  already contained; `CryptoActor` is not a vault for this category: the credentials agent
  stays the store. A secret that has reached the agent is registered with the `CryptoActor`, and an actor that
  needs it asks for a named operation with it, not for the plaintext (see "Named operations on registered
  secrets", below). Whether the `CryptoActor` also caches what it has decrypted or retrieved is an
  optimization (David, 2026-10-14).

**Local caching.** `CryptoActor` may cache what it learns (another
agent's escrow public key, once queried, does not need re-querying on
every subsequent local request to encrypt something to that same
target) — reducing redundant round-trips to the Credentials Agent
without weakening either sharing rule above.

**Relationship to the agent's own `TcpClientActor`.** Two distinct
entry points exist, for two genuinely different purposes:

1. **Direct actor → `CryptoActor` calls**, for credential retrieval
   and any standalone cryptographic operation an actor needs on its
   own behalf (an observer wanting the plaintext GitLab token it
   itself stored earlier, for instance).
2. **The "encrypt and send" flow**, which stays inside the familiar,
   already-existing `TcpClientActor` interface every actor already
   uses to publish anything. A requesting actor asks `TcpClientActor`
   to encrypt-and-publish a payload to a named recipient (and a
   topic/queue) exactly the way it already asks for an ordinary,
   unencrypted publish — one message, one actor to talk to, whether or
   not encryption is involved. `TcpClientActor` itself delegates to
   `CryptoActor` internally for the encryption step, then proceeds
   with its own, ordinary queue-management and delivery machinery
   unchanged.

**Fire-and-forget only, in both directions, between `CryptoActor` and
`TcpClientActor` — never a blocking call.** Every `TcpClientActor`
interaction anywhere else in this system has always been
fire-and-forget (`TcpClientMessage::publish`'s own doc comment: "Publish
is fire-and-forget at the `TcpClientMessage` level"); the actual
request/reply semantics this system needs have always lived one layer
up, via the correlation-ID/`Envelope<T>` pattern already established
throughout this document, never by one actor blocking on a
`ractor::rpc::call` to `TcpClientActor` itself. This matters
concretely here: a blocking call from `TcpClientActor` into
`CryptoActor`, where `CryptoActor` in turn needs to send a message
back through that same, now-blocked `TcpClientActor` instance (to
query a recipient's escrow public key it doesn't yet have cached, for
example) is a genuine deadlock — ractor's own sequential-mailbox
guarantee means `TcpClientActor` cannot process that inbound query
until its own, currently-paused `handle()` call returns, which is
itself waiting on `CryptoActor`, which is waiting on `TcpClientActor`
processing that very message. Kept strictly fire-and-forget in both
directions, with correlation-ID-based replies instead of blocking
awaits, this circular-wait shape cannot arise. No second Cassini
connection is needed to avoid it — one connection, one `TcpClientActor`,
shared safely by both the agent's own ordinary traffic and
`CryptoActor`'s own Credentials-Agent traffic, precisely because
neither side of that sharing ever blocks the other.

### Named operations on registered secrets (decided 2026-10-14, David; not built)

A secret that reaches an agent, in a signed document (`signed-configuration.md`) or from the credentials agent
(`RetrieveSecret`), is **registered** with the `CryptoActor` under its name, together with its **binding**: the
hosts it may be used for, and its kind. An actor that needs the secret does not ask for the plaintext. It asks
the `CryptoActor` to perform a **named operation** with it, for example "the git credential for host H", and
receives only the operation's result. The `CryptoActor` enforces the binding: an operation for a host the binding
does not name is refused. So the rule that a credential only goes to its own hosts is enforced where the secret
lives, and every use of a secret passes through one place. The operations are a fixed set the `CryptoActor`
implements, not closures supplied by callers, since a closure could simply return the plaintext.

- **The `CryptoActor` makes no network calls.** Operations that need I/O are carried out by other actors (an HTTP
  client actor, a GraphQL actor, and so on, each added when a use needs it). Such an actor is mostly pure
  decision logic around a thin effectful edge: preparing a request from the operation, the secret and the
  binding, and interpreting the response, are pure functions, and only sending is the edge. The key-holding
  actor therefore has no network surface.
- **Git is different.** libgit2 does its own network I/O and asks for a credential through a callback, so for git
  the operation hands the caller a short-lived username and token (wiped when dropped) for the host the binding
  names.
- **Escrow.** A registered secret and its binding are escrowed together with the credentials agent
  (`StoreSecret`, retried until acknowledged), so a restarted agent gets both back with `RetrieveSecret`.

Proposed, not yet confirmed: a result that is itself a secret (a token from a login) is registered under a new
name with an expiry and without escrow, and later operations name it like any other secret; and the
`CryptoActor` never blocks waiting on an I/O actor (the deadlock above).

First built, for the git credential path: the registry entry with its binding and escrow, and one operation, the
git credential for a host. The I/O actors and derived tokens come later. The messages (decided 2026-10-14,
David):

| Message | Does | Returns |
|---|---|---|
| `RegisterSecret { name, secret, context, binding }` | Decrypts `secret` (the `EscrowedSecret` bytes) under `context`, keeps the value with its binding (`host_pattern`, `kind`, `username`), and starts escrowing both together under `<identity>/<name>` with `StoreSecret`, retried until acknowledged. | `Ok` once registered, without waiting for the acknowledgement |
| `LoadSecret { name }` | At start, asks the credentials agent for `<identity>/<name>` (`RetrieveSecret`) and registers what comes back, binding included. | whether it was found, or a policy denial |
| `GitCredential { name, host }` | Checks that `host` matches the binding. | a username and token for libgit2, wiped when dropped (the username defaults to `oauth2` for a token), or a refusal: unknown secret, host not bound, kind not usable for git |

`StoreSecret` and `RetrieveSecret` stay as they are, since the harness and the escrow-keypair flow use them.
Decrypting an `EscrowedSecret` under a context is internal to `RegisterSecret`: no public message returns the
plaintext of a registered secret.

## Escrow-key and certificate-lookup queries

Three related but genuinely distinct request/reply queries exist, all
following the protocol in
`docs/architecture/cassini-request-reply-protocol.md` exactly, and all
core-to-core only today (no observer proxy built for any of them yet
-- see "What is deferred," below).

**Credentials Agent's own escrow-key query**
(`credentials-escrow-key-requests` / `credentials-escrow-key-replies`):
any participant encrypting a secret to the Credentials Agent needs a
reliable way to learn its current escrow public key. Cassini has no
message retention (confirmed directly against
`TopicManager::handle_subscribe_request` -- subscribing only delivers
messages published from that moment forward), so a subscriber joining
after a one-time "publish key on startup" would receive nothing until
a future key rotation, which may be arbitrarily far away or may never
happen. Given how routinely agents restart in this system by design
(forced rejuvenation is a core security mechanism, not a rare event),
that would leave the overwhelming majority of agent startups with no
reliable way to learn the current key at all -- a periodic broadcast
would fight the grain of how this system already works besides
(request/reply is the native idiom here, not speculative publishing).
`EscrowKeyRequest` is genuinely empty -- nothing to validate, nothing
to check; any authenticated core service asking on this topic gets the
key, since a public key isn't a secret and needs no trust decision.

**Certificate-lookup query** (`credentials-certificate-requests` /
`credentials-certificate-replies`): the decrypt-reencrypt-destroy
relay described in "Secret escrow," above, needs the requesting
consumer's own current, real public *mTLS* certificate to re-encrypt
to for flows that genuinely want pod-instance-level targeting.
`CertificateRequest { identity }` answers with either the actual
signed PEM (`CertificateReply::Found`) or `CertificateReply::NotFound`
-- deliberately not distinguishing "never issued" from "revoked"; a
caller only needs to know whether there's a usable cert to encrypt to
right now, not why one might be missing. Backed directly by
`CredentialsState::certificate_for`, which guarantees at most one
non-revoked certificate can exist per identity at any time (see
"Source of truth," above, on reissue superseding a prior certificate).

**Agent escrow-key query** (built: `AgentEscrowKeyRequest`, which `CryptoActor` uses for `EncryptForRecipient`):
the analogous lookup for an *agent's own* escrow public key (its
service identity, per "Agent escrow identity," above), as opposed to
either the Credentials Agent's own escrow key or an agent's ephemeral
mTLS certificate. This is the key another agent actually encrypts
agent-to-agent payloads to -- and, per "CryptoActor," above, is what
`CryptoActor` itself queries (and caches) on behalf of the
"encrypt and send" flow.

Every one of these queries is internal-agent-only today, not proxied
through the observer the way issuance is -- an external, non-Cassini
caller has no current way to ask any of them. Extending the observer
to proxy these, the way it already does for issuance, is real,
undesigned future work, not a rejected idea.

## Crate layout

```
credentials-agent/
  common/     -- shared between observe and consume (message/event types)
  observe/    -- the low-trust HTTP gateway binary
  consume/    -- the high-trust binary (event log, graph projection,
                 cert-issuer forwarding, escrow key custody)
  admin/      -- polar-admin-grant, an operator-facing CLI publishing
                 Grant/Revoke events directly to Cassini
  fetcher/    -- user-cert-fetcher, the human self-service issuance
                 client (see "User self-service issuance" below)
  client/     -- credentials-agent-client, the observer's HTTP client
                 (built 2026-10-13, from fetcher's own): shared by the
                 fetcher and by a workload asking for its certificates
                 (polar-crypto's workload_certificates)
```

There is no single, shared client crate -- the original plan called
for one (`credentials-agent/client`), but what actually got built is
two independent, purpose-specific binaries instead: `admin` (an
operator's own tool, talking to Cassini directly, since an admin is
already an authenticated Polar participant by the time they use it)
and `fetcher` (a human's own tool, talking to the observer's HTTP
gateway, since a human isn't a Cassini participant yet at the point
they need this). Correlation-ID request/reply bookkeeping still lives
in `lib`/`polar` as originally planned, not duplicated in either.
`CryptoActor` itself (see above) is expected to live in its own,
standalone crate -- not `lib`/`polar`, and not credentials-agent-
specific -- mirroring `polar-health`'s own, already-established
pattern exactly: a dedicated crate any agent depends on and
`spawn_linked`s directly within its own supervisor, passing
agent-specific configuration through its own `Args` type the same way
`HealthCheckArgs` already does.

Two further extractions this introduces, worth naming explicitly since
they're refactors of existing code, not purely new additions:

- **The generic request/reply correlation-ID machinery itself is not
  credentials-agent-specific** and does not live in either `admin` or
  `fetcher` — it belongs in `lib`/`polar`, alongside the other
  genuinely cross-cutting primitives already there (`graph`, `health`,
  `cassini`, `events`), since `cassini-request-reply-protocol.md` is
  explicitly a general pattern for any future agent needing
  synchronous-feeling behavior over Cassini's pub/sub transport.
- **Keypair generation and CSR building already exist, correctly, in
  `cert-issuer/client`** — but the human-issuance flow (`fetcher`) now
  needs the same capability. Rather than have `fetcher` depend on
  `cert-issuer/client` directly (a backwards dependency direction,
  given the architecture's own goal of `cert-issuer` becoming a
  narrowly-reachable, increasingly internal service over time), this
  logic is extracted into a new, neutral shared crate, `csr-utils`,
  that both `cert-issuer/client` and `fetcher` depend on.
  `cert-issuer/client` itself is refactored to use it too, rather than
  remaining the sole owner of logic something else now also needs.
  `csr-utils` holds keypair generation, CSR building, and a generic
  "attach a custom attribute/hash to a CSR" primitive; the
  human-issuance flow's actual decision to hash *the access token
  specifically* and bind it via that primitive is flow-specific
  business logic, and stays in `fetcher`. The agent escrow-identity
  CSR flow (see above) is expected to reuse `csr-utils` the same way,
  from within `CryptoActor`.

## Architecture: observer and consumer

Like every other Polar agent, the Credentials Agent has two halves with
deliberately different trust levels.

**Consumer (high-trust)**: owns the actual state (the event log above)
and the escrow private key. Subscribes to grant, revocation, and
issuance events on Cassini, appends the authoritative record to the
log, and projects the current, derived state into Neo4j for
observability. Answers authenticated, high-trust questions — "what is
currently granted to this identity" — only to callers who have already
established their own trust (an mTLS-connected Polar participant), the
same bar every other consumer-side interaction in this system already
requires.

**Observer (low-trust)**: the only part of this whole system that a
human's own tooling ever talks to directly. Accepts external input —
a user's CSR-plus-token bundle, an external verifier's revocation-status
query, an external caller's identity-key query — and does the absolute
minimum: basic well-formedness checking, then forwards. It never
validates a token, never makes an authorization decision, never touches
signing key material or the escrow private key. That decision-making
stays entirely on the high-trust side. This mirrors every other observer
in Polar, whose job is to safely receive from an untrusted world without
itself being trusted to decide anything — the only difference here is
that this observer's untrusted world is a human at a keyboard rather
than an external system like GitLab or Jira.

Critically, this means `cert-issuer` — the one component that holds the
CA's live signing key — is never directly reachable by user-facing
requests at all. A human's tool talks to the Credentials Agent's
observer; everything else happens on the trusted side of the broker.

## Three independent lifecycles

### Admin bootstrap (one-time, per admin)

An admin proves themselves once, using a credential that already exists
independently of anything Polar-specific — a `kubectl`-admin-backed
Kubernetes credential. This goes through the Credentials Agent's
observer exactly like every other flow — there is no direct-to-`cert-issuer`
carve-out for admin bootstrap; the observer already forwards either
credential type (Kubernetes or IdP) blindly, and the trusted side
validates whichever one arrives via the same multi-issuer `oidc.rs`
logic already built for both. In exchange, the admin receives an
ordinary Polar client certificate — nothing more privileged than any
other agent gets, and no grant lookup is involved, since the Kubernetes
credential itself is what authorizes this one exchange. That cert is
what lets their own tooling connect to Cassini as a genuine,
mTLS-authenticated participant afterward.

This step exists because someone has to be first, with nothing else yet
able to vouch for them. It is not part of the steady-state flow below,
and does not recur unless a new admin needs to be established.

### Admin grants and revocations (repeatable, ordinary agent traffic)

Once bootstrapped, an admin's ongoing actions are unremarkable Cassini
publishes, using their normal client cert:

- **Grant**: `{ target_identity, permissions, granted_by, timestamp,
  reason }`, published to a `credentials-grants` topic.
  `target_identity` must be the exact claim value the relevant
  `IssuerConfig.workload_identity_claim` will later extract from that
  user's real token — the admin confirms this by querying the IdP
  directly for the account's current value before publishing, rather
  than guessing or reusing a stale value. A grant is single-use — see
  "Source of truth," above. `reason` is a closed `GrantReason` enum
  (`UserOnboarding`, `ServiceOnboarding`, `RoleChange`,
  `TemporaryElevation`, `CorrectionOfError`, `PolicyRequirement`), not
  free text — genuinely Polar-specific, with no RFC to anchor
  against, but held to the same closed-set discipline as
  `RevocationReason` below regardless.
- **Revoke**: `{ serial, reason, revoked_by, timestamp }`, published to a
  `credentials-revocations` topic, for either a human-issued or
  workload-issued certificate — revocation is uniform across both, since
  both are ultimately just certificates with serial numbers already
  recorded in the event log. `reason` is a closed `RevocationReason`
  enum (`Compromised`, `Superseded`, `Decommissioned`,
  `UserOffboarded`, `AccessNoLongerRequired`, `PolicyViolation`,
  `IssuedInError`), not free text -- loosely modeled on RFC 5280's own
  CRL revocation reason codes, adapted for Polar's domain (workload
  identities and human users alike, not just RFC 5280's original
  TLS-server-cert context). `Superseded` is system-generated only,
  never an admin's own choice -- see the reissue behavior in "Source
  of truth," above.

The Credentials Agent's consumer appends both to the event log and
updates the Neo4j projection accordingly. A revoked certificate record
is marked in place, not removed -- consistent with how this system
treats history everywhere else -- so it, and its own revocation
audit trail, survives.

### User self-service issuance (steady state, fully automated)

This is the flow that actually issues a certificate to a human, and it
requires no admin presence at the time it happens — only a prior grant,
made independently, at any earlier point.

1. The user authenticates against the IdP directly (Authorization Code
   flow) and receives a real access token.
2. Their tool generates a keypair locally (via `csr-utils`) — the
   private key never leaves the machine — and builds a CSR. The CSR
   embeds a hash of the raw token as a custom attribute (via
   `csr-utils`'s generic attribute-binding primitive), cryptographically
   binding the two artifacts together: a CSR generated in possession of
   one token cannot later be silently paired with a different one.
3. The tool sends the CSR-plus-token bundle to the Credentials Agent's
   **observer** — not to `cert-issuer`, and not to the consumer side.
   The observer acts as a synchronous-HTTP-to-async-Cassini gateway,
   per `cassini-request-reply-protocol.md`: it holds the connection
   open, generates a `correlation_id`, publishes to
   `credentials-issuance-requests`, and subscribes to
   `credentials-issuance-replies` filtering for that ID.
4. On the trusted side: the token is validated for real (the existing,
   already-tested multi-issuer `oidc.rs` logic — signature, issuer,
   audience, expiry) and, since local signature validation alone cannot
   detect a session that was revoked or logged out server-side before
   natural expiry, corroborated with a live call to the IdP's token
   introspection endpoint (RFC 7662). The Credentials Agent looks up
   whatever grant exists for the now-validated identity, and checks
   that no currently-valid certificate already exists for this exact
   `(identity, instance_binding)` pair (see "Pods cannot request their
   own renewal," below). `cert-issuer` combines identity (from the
   token, used unmodified) with permissions (from the grant) into the
   certificate's SAN set, and signs with its own key — no delegated
   signing authority is needed for this path, since the admin's earlier
   grant is what authorizes the request, not the admin's presence at
   request time.
5. The reply — the signed certificate, or a refusal, tagged with the
   original `correlation_id` — also includes the Credentials Agent's
   current escrow public key, so the user's tool has it immediately
   without a separate round-trip.
6. The signed certificate is published back through Cassini, through
   the Credentials Agent, and back to the user's tool — the same
   asynchronous path in reverse. Per-attempt timeout and retry policy
   (up to 3 attempts, 5-minute hard ceiling) follow
   `cassini-request-reply-protocol.md` exactly.
7. The issuance itself is appended to the event log — the same shape
   whether it came from this path or from a workload's `/issue` call —
   giving the Credentials Agent complete visibility into every
   certificate that has ever been issued, and therefore a complete,
   authoritative revocation list.

### Automated agent escrow-identity issuance (steady state, fully automated)

The third lifecycle, alongside admin bootstrap and user self-service
issuance -- see "Agent escrow identity" and "Agent escrow bootstrap,"
above, for the full design. In brief: `CryptoActor`, on the affected
agent's own behalf, first attempts to retrieve a previously-escrowed
keypair for its own service identity; only if none exists does it fall
into issuance -- generating its own escrow keypair locally, building a
CSR via `csr-utils`, and submitting it, authorized purely by its own
already-validated service identity (no separate grant lookup, the same
reasoning admin bootstrap already uses). The Credentials Agent embeds
a permission set into the resulting certificate automatically, playing
the "admin" role itself for this one flow -- uniform across all agents
today, with a named path to becoming agent-specific once the
Credentials Agent can source real per-agent configuration from the
scheduler. `CryptoActor` then immediately escrows the resulting
keypair through the same general `EscrowedSecret` mechanism this
document already describes, so the next rejuvenation retrieves its
service's existing escrow identity rather than generating a new one
and orphaning any ciphertext already sent to the old one.

## Core services vs. everything else

Two distinct populations exist for `polar-init`'s purposes, driven by a
new flag (not a new binary — `polar-init` stays entirely CLI/env-driven,
consistent with its existing design) pointing it either at `cert-issuer`
directly or at the Credentials Agent's observer.

**Core services — Cassini and the Credentials Agent themselves —
continue talking to `cert-issuer` directly.** This is not a convenience
choice; it is structurally required. The observer-relay path *is*
Cassini being up and the Credentials Agent being a working participant
on it. Cassini needs its own cert before any broker exists to relay a
request through; the Credentials Agent needs its own cert before it can
act as a Cassini participant at all — bootstrapping through its own
observer would be circular. Every other service's direct access today
is incidental, not load-bearing, and can move to the relay path without
issue.

**Everything else talks only to the Credentials Agent's observer.**
The observer does not need to distinguish a human's IdP token from a
workload's Kubernetes token to do this correctly — it forwards either
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
repeatedly during this project's own development.

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
**observer's** responsibility, not a convention: the observer checks
incoming requests against the Credentials Agent's own issuance records
(derived from the event log), keyed on `(identity, instance_binding)`
together, and refuses any request where that exact pair already holds
a currently valid, unrevoked certificate.

**Which identity.** A credential has two names, and the ledger keeps each where it belongs (decided 2026-10-14, after
the secret harness showed `RequesterCertificateNotFound` for a pod whose certificate had been issued through the
credentials agent). The **certificate's identity** is the normalized one, its DNS name (`name.ns.serviceaccount.<domain>`
for a workload, `claim.scope` for an identity-provider credential, the account's UUID for an account). It is what the
mTLS connection says, so it is what every lookup uses: `IssuanceCompleted.identity`, the renewal check above, the
requester of a retrieve, store or decrypt, the owner of a secret, the recipient of an encrypted payload. The
**credential's own identity** (the token's raw one) is what a *grant* is written under, because an administrator names
the identity provider's claim before any certificate exists. The issuance event is where the two meet, so it carries
the second only when a grant was found, as the optional `grant_key`; the fold uses up the grant under
`grant_key`, or under `identity` when there is none, which is how every event written before the field existed reads
(`CredentialsEvent::consumed_grant_key` is the one derivation, used by state and the graph projection). Both issuance handlers (the pod's client and server certificates, and the service identity certificate that carries
its escrow key) ask cert-issuer for the normalized identity and refuse a credential they are not given one for, since recording
the raw name would put the certificate where nothing looks for it. The properties are unchanged: a grant is single-use
and used up at issuance, a pod instance cannot renew itself (the check is still `(identity, instance_binding, type)`),
and the normalized identity comes from cert-issuer's own validation of the token, never from the caller.

`instance_binding` is `IssuerConfig.instance_binding_claim`, present
since v1 but previously unused beyond audit telemetry (for Kubernetes,
the requesting pod's own UID). Scoping the check to the pair, not
identity alone, is what makes this safe rather than disruptive:

- **Rolling restart**: an old pod and a new pod briefly coexist during
  a normal Deployment update, both under the same ServiceAccount and
  therefore the same identity. Different UID, since it is a genuinely
  new pod — the new pod's first request is correctly allowed even
  while the old pod's cert is still valid.
- **Health-check-triggered rejuvenation**: `restartPolicy: Never` plus
  the Job controller mean a `polar_health` deadline firing does not
  restart a container in place — it fails the pod outright, and the
  Job controller schedules an entirely new one. (The ending should be an
  orderly shutdown with exit code 1; see `health-and-shutdown.md`.) New pod, new UID, the
  same mechanism as a rolling restart, with no special-casing required.
- **A pod requesting its own renewal**: same identity, same UID, cert
  already valid — correctly refused. This is the actual defensive
  case the rule exists for.

**Per certificate type (2026-10-13).** A pod instance may need two certificates at start: a client
certificate, and for a listener a server certificate. The rule is therefore keyed on
`(identity, instance_binding, type)`: a pod instance may be issued one client certificate and one server
certificate and is refused a second of either (`AlreadyHasValidCertificate`), and a reissue supersedes only the
prior certificate of the **same type**. The request says which it wants (`cert_type`, default client, so existing
callers are unchanged) and, for a server certificate, the DNS names (`names`); only a Kubernetes credential may ask
for a server certificate, a client request carries no names (its extras are the grant's permissions, added from the
ledger), and only a client certificate uses up a grant. The residual this leaves, stated plainly: a pod that has a
client certificate and no server certificate could ask for a server one. It has no token to ask with (the main
container never holds it), and cert-issuer's per-workload name allowlist decides what any server certificate may
carry. The service-identity (escrow) certificate is a different thing with its own flow and record, and is not
requestable here.

## What is deferred

- OCSP, as a live alternative or complement to a periodically-published
  CRL. The CRL is the simpler mechanism to get working first; OCSP is a
  documented, real future upgrade, not a rejected idea.
- `CryptoActor`'s delegation to `TcpClientActor` for the "encrypt and send" flow (publishing a payload
  encrypted to a named recipient in one call). The `CryptoActor` itself is built.
- Sourcing agent-specific (rather than uniform) permission sets for
  automated agent escrow-identity issuance from the scheduler agent,
  once the scheduler's own credential-schedule work exists.
- Extending the observer to proxy the escrow-key and certificate-lookup
  queries (and the agent escrow-key query) for external,
  non-Cassini callers, the way it already does for issuance -- all are
  internal-agent-only today.
- A real "infrastructure failure vs. deterministic refusal" reply
  variant for `IssuanceReply`, mirroring the distinction
  `StoreSecretReply::Failed` already draws -- today, a signed
  certificate that fails to append to the ledger is still reported to
  the caller as `Issued`, the same shape of gap `StoreSecretReply`
  was corrected to avoid.
- A checkpoint/compaction mechanism for the event log, if unbounded
  growth ever becomes an actual, measured problem.

## Built since this section was written

Several items above were listed as deferred and have been built (checked against the code, 2026-10-14):

- the general secret-retrieve flow: `RetrieveSecret` (`retrieve_secret.rs`, with the policy check);
- `CryptoActor` (`polar-crypto`), with kernel-keyring custody and an in-memory fallback, and its cache of other
  agents' escrow public keys;
- the agent escrow-identity issuance flow and the agent escrow-key query (`agent_escrow_issuance`,
  `agent_escrow_key`);
- the ephemeral key exchange for the first retrieval of one's own escrow keypair (`ephemeral_exchange`);
- the init container's escrow-key bootstrap and hand-off of a key handle to the workload (`EnsureBootstrapped`,
  `POLAR_ESCROW_KEY_HANDLE`).

## See also

- `docs/architecture/cassini-request-reply-protocol.md` — the
  correlation-ID request/reply mechanism used by both the issuance and
  identity-distribution flows above.
