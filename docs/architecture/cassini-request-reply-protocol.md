# Request/Reply Over Cassini — A Client-Side Convention

## Why this exists

Cassini's real client protocol (confirmed directly against `cassini-client`'s
source) offers no native request/reply primitive — only `send_message`,
publishing to a topic. Several real interactions need to *feel* like a
synchronous call anyway: a user's tool waiting for a signed certificate,
one internal service waiting on another's answer. This document is the
convention that gets that behavior on top of a pub/sub-only transport,
worked out in full for the Credentials Agent's user-issuance flow — but
written generically, since any future agent needing the same shape of
interaction should reuse this rather than reinvent it.

This adds no new capability to Cassini itself. It's a pattern for how
clients already using `send_message`/subscribe choose to structure their
own traffic.

## The core mechanism

1. The requester generates a fresh `correlation_id` (a UUID) for this
   specific request — not reused across retries; see "Retries," below.
2. The requester publishes its request, carrying that `correlation_id`
   as a field in the message payload.
3. The requester subscribes to the relevant reply topic (see "Topic
   naming," below) and waits, filtering incoming messages for the one
   whose `correlation_id` matches.
4. The responder does the matching publish: same `correlation_id`,
   carried through unchanged, to the reply topic.

No change to Cassini's broker or protocol. This is purely a payload
convention plus ordinary topic subscription, layered on primitives that
already exist and work today.

## The synchronous-facing gateway pattern

For a caller that has no Cassini identity of its own yet — the
Credentials Agent's user-issuance flow is the motivating case: the
user's tool has no certificate to authenticate with, since obtaining
one is the entire point of the request — the actual Cassini participant
is a separate, already-connected service acting as a gateway:

- The external caller makes an ordinary synchronous call (HTTP, matching
  how `cert-issuer`'s own `/issue` endpoint already works) to that
  gateway.
- The gateway holds that connection open. It generates the
  `correlation_id`, publishes the request, and subscribes to the reply
  topic on the caller's behalf.
- When the correlated reply arrives (or the timeout below fires), the
  gateway completes the still-open HTTP response and the underlying
  Cassini subscription is torn down.

This is the Credentials Agent observer's actual role in the
user-issuance flow: a synchronous-HTTP-to-async-Cassini gateway, holding
one external connection open per in-flight request while the real work
happens over Cassini underneath it.

## Timeout

A request that never gets a reply — the responder is down, degraded, or
the request was silently lost — must not hang the caller indefinitely.
The timeout for a given request is computed dynamically, not a fixed
constant, specifically for interactions gated by a bearer token's own
validity window (the Credentials Agent's user-issuance flow, again, is
the motivating case):

per_attempt_timeout = min(token_remaining_lifetime / 3, 100 seconds)


Computed by peeking the token's unverified `exp` claim — reading it
without checking the signature, purely to size a timeout, the same
category of operation as `oidc.rs`'s existing `peek_iss()` (used for
issuer routing before real validation happens). This is safe for a
low-trust component to do: an operational read of a claim's value, not
a trust decision, and the value is only ever used to bound how long to
wait, never to authorize anything.

Dividing by three specifically anticipates the retry policy below: three
attempts within roughly one token's remaining lifetime, rather than
racing to use up nearly all of it on a single attempt.

## Retries

Ownership of retry policy belongs entirely to the requester (the calling
tool), not the gateway — the gateway's job is exactly one bounded
attempt, nothing more:

- On a timeout, the requester retries, up to **3 total attempts**.
- Each retry is a genuinely new request: a fresh `correlation_id`, a
  fresh publish. A timeout could mean the original request was lost in
  transit, not just that the reply is slow — re-listening on the old
  `correlation_id` would wait forever for a reply that may never come.
- The same token is reused across retries as long as it's still valid;
  no fresh IdP login is needed between attempts.
- **Hard ceiling: total wait time across all attempts must not exceed 5
  minutes.** In the Credentials Agent's case this holds by construction
  — `100s` per-attempt cap × 3 attempts = 300s exactly — rather than
  needing separate enforcement logic.
- After the final attempt still fails, the requester surfaces a clear,
  actionable error rather than retrying further.

## Topic naming

Established for the Credentials Agent's user-issuance flow, and the
convention any future adopter of this pattern should follow: `<subject>-requests`
for the forward leg, `<subject>-replies` for the reply leg. Concretely:

- `credentials-issuance-requests` — observer → consumer
- `credentials-issuance-replies` — consumer → observer
- `credentials-workload-issuance-requests` — observer → consumer: a workload's own request for its pod-instance
  certificates, before it has any certificate; either an open-session request or a request with the service-account
  token sealed to an open session (`credentials-agent-common/src/workload_issuance.rs`)
- `credentials-workload-issuance-replies` — consumer → observer: the session offer, or the usual issuance reply

## What this does not solve

- **Message ordering or exactly-once delivery.** This pattern assumes
  Cassini's own existing delivery guarantees; it adds nothing beyond
  matching a reply to its request via `correlation_id`.
- **Multiple concurrent requests from the same gateway instance.** The
  gateway must track its own set of in-flight `correlation_id`s (one HTTP
  connection held open per request) — this document doesn't prescribe
  that bookkeeping's implementation, only the wire-level convention it's
  built on.
