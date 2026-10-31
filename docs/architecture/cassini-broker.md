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

# The Cassini broker, as built

Read from the code on 2026-10-21 (`src/agents/cassini/broker`, paths below relative to `src/agents/cassini/`). How
clients connect, register, subscribe and publish; where the client's identity is known; what the broker refuses today
and what a client learns of it. The design for deciding who may publish and subscribe, and the gaps it closes, is in
`policy-enforcement.md`. The request/reply convention is in `cassini-request-reply-protocol.md`; delivery and
retention in `cassini-client-reliability.md`.

## Actors

Registry names are in `broker/src/lib.rs:41-46`.

- **Broker** (`broker.rs:55`) starts its infrastructure with `ServiceBuilder` (220-227), then the managers below
  (238-355).
- **ListenerManager** (`listener.rs:120`) runs the TLS accept loop (234-327) and spawns one **Listener** per connection,
  named by a random client id (284, 306).
- **SessionManager** (`session.rs:46`) holds `Session { agent_ref, identity, detached_at, superseded, .. }` (49-60) and
  spawns one **SessionAgent** per registration, named by its registration id (648-658).
- **TopicManager** (`topic.rs:44`) spawns one **TopicAgent** per topic, registered under the bare topic string (77-81),
  on the first publish or subscribe (`ensure_topic`, 61). Topics are exact strings: there are no wildcards.
- **SubscriberManager** (`subscriber.rs:46`) spawns one **SubscriberAgent** per session and topic, named
  `<registration_id>:<topic>` (`lib.rs:66-68`, `subscriber.rs:411-445`).
- **ControlManager** (`control.rs`).

## Identity

*Brought up to date 2026-10-28; it described the broker before stage 5b, when the certificate was dropped after the
handshake.*

The handshake is mutual TLS: the client's certificate must chain to `TLS_CA_CERT` (`WebPkiClientVerifier`,
`listener.rs:215`). Right after it, before the stream is split, the Listener reads three things from the leaf, once
(`listener.rs:317-319`):
- **the identity,** its first DNS SAN (`extract_peer_identity`, `listener.rs:93`), `None` when it has none;
- **the serial,** as the credentials agent's ledger writes it, so a revoked certificate's sessions are refused and ended
  (`extract_peer_serial`, `listener.rs:125`);
- **the floor,** what the certificate admits where no policy speaks: its declared topics (`p:pub:`/`p:sub:` URI SANs)
  and those its workload and `g:` groups imply (`extract_peer_floor`, `listener.rs:136`). A certificate that cannot be
  read gives a reason, and every publish and subscribe on that connection is refused with it.

| Where | Has the client's identity |
|---|---|
| Listener | yes, `state.peer_identity` and `state.floor`; every publish and subscribe is decided there (`decide`, `listener.rs:694`, from `handle_publish_request` and `handle_subscribe_request`) |
| SessionManager | as `Session.identity` (`session.rs:56`) |
| Broker, TopicManager, SubscriberManager | no: a subscribe carries the registration id and topic; a publish carries `published_by` only when the publisher shares it |

`published_by` is the verified identity, stamped by the broker on a publish when the client sets `share_identity` (the
default). Receivers check it where it matters (`AuthorityCore`'s publisher check; the broker believes authority only
from `POLAR_AUTHORITY_PUBLISHER`).

*Resuming a session* is the identity's alone: a connection asking to resume a session of another identity is given a new
one (`may_resume`, `session.rs:1537`, used at `:631`), so a registration id is not a bearer token. (The flaw this section
used to record, fixed.)

## The flows

- **Register:** Listener (960) → Broker (`broker.rs:679`) → SessionManager (`session.rs:558`): reuses a session
  (595-641) or creates one with the peer identity (662-670) and supersedes older ones (679-683); the Listener stores the
  registration id and the session's address (`listener.rs:1011-1059`).
- **Subscribe:** Listener (1279) → SessionAgent (`session.rs:1236`) → Broker (`broker.rs:724`) → TopicManager
  (`topic.rs:482`, `ensure_topic_and_subscribe` 103) → SubscriberManager `CreateSubscriber` (`subscriber.rs:391`) →
  TopicAgent `AddSubscriber` (`topic.rs:797`).
- **Publish:** Listener (1119) → SessionAgent (`session.rs:1068`) → Broker (`broker.rs:804`) → TopicManager (397) →
  TopicAgent (737), which delivers to each SubscriberAgent or queues, and acknowledges to the session (784-790).
- **Unsubscribe:** the SubscriberManager stops the subscriber (`subscriber.rs:474`); the TopicManager has no arm for it
  (logs "unexpected", `topic.rs:268-337`) and the TopicAgent keeps the dead reference.
- **Disconnect:** EOF or an I/O error sends `DisconnectRequest` (`listener.rs:722-772`); a goodbye ends the session, a
  dropped connection detaches it for the retention window (`session.rs:208-339`).

## One session per identity, and fan-out

Read 2026-10-22, after stage 5a found the first while measuring (`broker-baseline.md`, finding 6).

- **The supersede rule.** A new session of an identity takes over every older session of it, including one whose
  client still looks connected (`supersede_older`, `session.rs:346`). The older one can no longer be resumed (597); its
  connection is closed, all but the new client's (`CloseConnection`, 854); its subscribers stop forwarding and hold
  what arrives (`hold_subscriptions`, `subscriber.rs:208`), and it stops once they all do (`complete_takeover`). When
  the new session subscribes to a topic it inherits what the superseded sessions of its identity held on it, oldest
  first (`claim_held`, `subscriber.rs:237`); what nobody claims is let go a retention window after the takeover
  (`end_expired_sessions`). An instance that lost its session and asks to resume it is given a new one that supersedes
  nothing, so two live instances do not take turns closing each other (682). Tests: `broker/tests/supersede.rs`.
  *Since 2026-10-27* the superseded session is drained, not stopped, once its subscribers hold: a stop is a signal,
  ahead of the queued `CloseConnection`, so a session with nothing to hold ended before closing its connection and left
  the old instance connected with no session.
- **Revoked certificates (2026-10-27, `key-custody.md`, step 5).** The listener reads the leaf's serial at accept. A
  certificate the credentials agent's revoked set names is refused at registration (`cassini_types::Refusal::revoked()`:
  the code `RefusalCode::Revoked`, which a reader acts on, and the words `REVOKED_REFUSAL`, for people; since
  2026-10-28, when an older daemon matching the words did not hear its revocation once they changed), and
  on each change of authority every connection whose certificate is now revoked is closed; the session is kept for the
  retention window, for the identity's next certificate to claim. Tests: `broker/tests/revocation.rs`,
  `custody_revocation.rs`. (Until 2026-10-28 a revoked certificate could still register a watcher: below.)
- **Why:** a replaced pod (a rollout, a Job retry) usually registers before the broker sees the old connection is dead.
  The replacement takes over and inherits what was waiting for the old one.
- **Fan-out.** A topic delivers every message to every subscriber (`topic.rs:728`). There are no consumer groups.
- **So a workload cannot scale out.** The identity is the workload's (the certificate's first DNS SAN), so its pods
  supersede each other. Identities per pod would stop that but would not share the work: with fan-out, every replica
  receives every message (two `kube-consumer`s would both write each to the graph). Scaling a consumer or processor out
  needs both a way to tell a replica from a replacement and delivery shared within a group; an observer that scales out
  has to split its source, which the broker cannot do for it. Open, in `remaining-work.md`.

## Watcher sessions (2026-10-27; retired 2026-10-28)

From 2026-10-27 to 2026-10-28 the broker had a second, limited kind of session, a *watcher*, for the custody daemon:
the daemon then held the pod's key and connected as the pod, so its session and the workload's would have superseded
each other in turn, and it had to hear the pod's revocation whatever the workload did. A watcher was declared at
registration (`RegistrationRequest { watcher }`), held to the authority topics, left open when its certificate was
revoked, and capped at one per certificate with a 30-second tenure, because the broker could not tell the daemon's
connection from a compromised workload's.

The custodian identity (`key-custody.md`, "The custodian identity") removed the need. The daemon's session is now its
custodian's, `custody.<X>`, under a certificate of its own whose key only the daemon signs with (`Slot::Custodian`), so:
- it neither supersedes X's session nor is superseded by it: they are two identities, under the rules every session
  has (one per identity, the newest superseding; a resumed session superseding nothing);
- its floor is the custodian's (`CUSTODIAN_PUBLISHES`, `CUSTODIAN_SUBSCRIBES`, implied by the identity), so nothing
  limits it that the floor does not;
- a revoked custodian is refused and closed like any revoked certificate, and the daemon, refused as revoked
  (`ClientEvent::RegistrationRefused` with `RefusalCode::Revoked`, whatever its words), destroys every key and exits; X
  revoked leaves the custodian's session open to
  hear it;
- a workload cannot open a session as its custodian (it cannot sign as one), so there is nothing to cap.

Removed (2026-10-28): the `watcher` field of both `RegistrationRequest`s and `peer_serial` of the broker's,
`TCPClientConfig::as_watcher`, `authority::watcher_may` and `WATCHER_REFUSAL`, the revoked-registration exception,
`session::WATCHER_PLACE_HELD`, `replace_older_watchers`, `BrokerArgs::watcher_tenure` and `DEFAULT_WATCHER_TENURE`,
and `broker/tests/watcher.rs`. The end-to-end test that a compromised workload's sessions cannot keep the daemon from
its revocation stays, as sessions the workload opens beside its custodian's
(`custody_revocation.rs`, `sessions_the_workload_opens_do_not_keep_its_custodian_from_its_revocation`).

## What is refused, and what the client learns

- The Listener refuses a publish or subscribe with no session or a mismatched one, and writes the refusal itself
  (`listener.rs:1135-1163`, 1292-1345). **This is the natural place for an authorization decision:** it has the identity
  and the writer.
- The broker's shutdown refusals go to the SessionManager, which has no arm for them and drops them
  (`broker.rs:694-840`, `session.rs:259`). A subscribe is never acknowledged on success: `SubscribeAcknowledgment` is only
  sent for errors, and `ClientEvent::ClientSubscribed` is never emitted.
- The one refusal that works end to end is a control `PrepareForShutdown` with the wrong token (`control.rs:414-427`).
  The other control operations (list or disconnect sessions) authorize nothing.
- **On the client** (`client/src/lib.rs`), an `Err` acknowledgment or `ErrorMessage` is a `warn!` and nothing more
  (700-748); none becomes a `ClientEvent`, so neither an agent nor a test can see a refusal. An `ErrorMessage` containing
  "session mismatch" makes the client register again, so a denial must not say that. Subscriptions are sent again at
  every registration (`polar-agent-builder/src/lib.rs:617-660`), so a refused one is retried each time.
- **No authorization of publish or subscribe exists:** any holder of a certificate from the cluster's CA may publish or
  subscribe to any topic (`remaining-work.md`, the gap of 2026-10-14). *Built 2026-10-22 (stage 5b): see "Policy in the
  broker", below. A refused publish is a `PublishResponse` with `Err` and an empty payload, a refused subscription a
  `SubscribeAcknowledgment` with `Err`; the client turns them into `PublishRefused` and `SubscribeRefused` (stage 0).
  Some of the points above are older than stage 0, which made a subscribe acknowledged and refusals events.*

## Policy in the broker

*Built 2026-10-22 (stage 5b), and not yet run on a cluster:*

- **Its authority.** The broker starts an `AuthorityHandle` of its own under the Broker actor, its policy and
  permissions actors registered as `cassini.policy` and `cassini.permissions` (`authority.rs`, `BrokerAuthority::spawn`,
  started in `broker.rs` before the ListenerManager, which now comes last so no connection is accepted before it
  exists). It believes only `POLAR_AUTHORITY_PUBLISHER` (the builder's variable, default the credentials agent's
  identity), and asks for the current authority at start and whenever its authority wants to, by publishing
  `AuthoritySnapshotRequest` on `polar-authority-snapshot-requests` through the TopicManager under the registration
  id `cassini` (no client has it).
- **Hearing it.** A publish on `polar-authority-updates` or `polar-authority-snapshot-replies` that the broker admits
  is delivered as any other and also handed to its authority, as published by the identity the Listener verified,
  never by what the client says (`listener.rs`, `handle_publish_request`, `BrokerAuthority::hear`).
- **The floor.** Read once per connection, right after the handshake, from the leaf certificate
  (`extract_peer_floor`, `polar_agent_builder::Floor`): the identity (first DNS name), the `p:pub:`/`p:sub:` names, and
  the topics `topics::implied_topics` gives the identity's workload and the certificate's `g:` groups. An identity that
  names no workload (a person's) implies nothing. A certificate that cannot be read makes every publish and subscribe on
  the connection refused, with the reason.
- **The decision.** The Listener decides each publish and subscribe after its session checks (`Listener::decide`):
  `BrokerAuthority::decide` asks `cassini.policy` with `polar_agent_builder::topic_gate::ask` and applies the builder's
  own `rule`, so a `permit` admits beyond the floor, a `forbid` refuses within it, silence or no policy leaves it to
  the floor, and no answer refuses. Every refusal starts "the broker refused:" and never contains "session mismatch".
- **The cache.** Each Listener keeps its own decisions (`DecisionCache`, `authority.rs`), keyed by topic and access,
  refusals as well as permissions, for `CASSINI_DECISION_CACHE_SECS` (default 5; `0` keeps none, the switch the
  measurement uses). A change of authority (the `AuthorityChanged` its `AuthorityHandle` announces when a verified
  update or snapshot is applied) adds one to a count every Listener reads (`ChangeCounter`), and a decision made under
  an earlier count is a miss, so every connection's cache is emptied at once with no lock and no message to each. The
  count is read before deciding, so a change while a decision is made leaves it stale. At most 1,024 decisions are
  kept per connection; past that they are dropped and made again. A policy given straight to the policy actor (as the
  tests' `set_policy` does) is not a change: it applies when the kept decisions run out. Locally, the four-publisher
  load test (20,000 messages) took 12.5 s deciding every publish and 1.5 s with the cache. Tests: the cache's own
  (`authority.rs`, with a property that no answer outlives its time or a change), and three in `enforcement.rs` (no
  cache: the next publish; a kept decision lasting its time; a change ending it at once, with a ten-minute cache).
- **Held subscriptions, judged again.** Each Listener remembers the subscriptions the broker acknowledged. After a
  change of authority is counted, `ChangeCounter` tells the ListenerManager, which tells every Listener
  (`BrokerMessage::JudgeSubscriptions`), and each decides its own again (the count has moved, so none comes from the
  cache). One now refused is dropped (`Listener::drop_subscription`): the session unsubscribes, and the client gets an
  unasked `SubscribeAcknowledgment` with `Err`, which it reports as `SubscribeRefused`; the acknowledgment of that
  unsubscribe is not passed on. A resumed session's subscriptions, made under the certificate it had, are listed by the
  SubscriberManager (`BrokerMessage::SubscriptionsOf`) once the new connection has the session, and judged the same
  way. Because a resumed session is replayed what was held for it before that judgment, and messages can be on their
  way when a subscription is dropped, every delivery is written only on a topic the connection may still subscribe to
  (`handle_publish_response`, the cache answering), and a delivery on a refused topic drops its subscription. A
  decision that could not be made (the policy actor not answering) refuses, as everywhere, so it drops a held
  subscription too. Tests: in `enforcement.rs`, a new forbid dropping a held subscription and the client told; one the
  new authority still admits kept; a resumed session whose renewed certificate carries fewer topics losing the rest,
  with a message held for it and with nothing held.
- **A trap found building it:** the decision waits on the policy actor, and the Listener's handlers hold an entered
  span; a span held across a wait broke the tracing registry's per-thread stack when the task resumed on another thread
  (a panic in `tracing-subscriber`'s `sharded.rs`, and two in five messages lost in the four-publisher load test). The
  decision runs with the span left and instrumented by it. The Listener's writes still waited inside entered spans, and
  so did the other managers' handlers; under load they failed the same way (2026-10-30). Every handler now
  instruments its future (`async { .. }.instrument(span).await`) and holds no guard across an await, and
  `clippy::await_holding_invalid_type`, configured in every `clippy.toml` with `tracing::span::Entered` and
  `EnteredSpan`, refuses one.
- **Tests:** `broker/tests/enforcement.rs` (declared admitted and undeclared refused with no policy; implied topics;
  a certificate naming nothing; a Cedar forbid and permit; a snapshot believed from the credentials agent and not from
  an impostor allowed to publish on the topic). Every other broker test starts its broker with a policy that permits
  everything (`support::OPEN_POLICY`); `TestBroker::start_enforcing` starts one without.

Before stage 5b: the broker is built with `ServiceBuilder` (`CASSINI_CLIENT = false`, `broker/src/agent.rs:44-53`), so it has a health
check but no `AuthorityHandle`: no PolicyActor, no PermissionsActor, no authority updates
(`polar-agent-builder/src/infrastructure.rs:614-633`). It cannot be a client of itself (its certificate is a server
certificate). `AuthorityHandle::spawn` is public, so the broker can start one itself and feed it the authority topics'
publishes in process, with the publisher's identity from its own Listener.

## Bounds and flow control (as built, 2026-10-30)

*Built the same day: "The bounds, as built", below, the plan in cassini-backpressure.md. What follows is the broker
before them, for the record.*

Nothing bounds what the broker holds, by count, bytes or time, except the session retention `R`.

- **The acknowledgement is acceptance, not delivery:** the topic acks a publish once it has cast it into each
  subscriber's mailbox, or queued it when there is none (`broker/src/topic.rs:732-772`). Every hop after the topic is an
  unbounded ractor mailbox, and the subscriber's Listener awaits its socket write inside its handler, with no timeout
  (`broker/src/listener.rs:751-781`), so a subscriber that is slow but connected grows the broker without limit. Measured
  2026-10-30 at 64 KiB saturated: the backlog for a sink that drained about 11% slower than the broker accepted was 46 to
  176 MB of heap in under a second (broker-baseline.md, the fourth set, on `feat/musl-mimalloc` until it merges).
- **Frames:** no maximum. The read task takes a `u32` length and allocates `vec![0u8; len]` (`listener.rs:857-898`), so
  one frame can ask for 4 GiB; a zero length is a keepalive.
- **Connections:** no limit on their number or rate; each accept is a task and a Listener (`listener.rs:271-369`).
- **The read loop cannot be paused:** a detached task decodes each frame into the Listener's unbounded mailbox
  (`listener.rs:850-934`); its only control is abort.
- **Holders:** the topic queue for no subscribers is unbounded in size and drops by age after 180 s, checked every 60 s
  (`topic.rs:37-41, 530, 847-870`); a subscriber's dead-letter queue (`subscriber.rs:643`) is unbounded, with no expiry
  of its own, until its session is swept at `R` (600 s, `lib.rs:40`); replays to a resumed subscriber are a burst.
  *(Since 2026-10-30 the dead-letter queue is the subscriber's delivery outbox, which holds every delivery until its
  client acknowledges it: cassini-client-reliability.md, "Step 5's acknowledged deliveries, as built".)*
- **Counters:** the session's `pending_publishes` (`session.rs:1137-1145, 1270-1276`) counts publishes between session
  and ack, for the shutdown drain only.
- **Sockets:** keepalive and `TCP_USER_TIMEOUT` 60 s on both ends (`cassini/types/src/socket.rs:61-77`); no
  `TCP_NODELAY`, no buffer sizes; writers are `BufWriter` behind a `tokio::Mutex`.
- **During shutdown** a publish is dropped with no ack and no refusal (`session.rs:1127-1130`), and subscriber
  dead-letter queues are not flushed.
- **Tests:** none exercises a slow consumer; `TestClient` (`broker/tests/support/mod.rs:634-920`) can stop (close)
  but not stay connected and stop reading, and the harness sink cannot be slowed (`test/sink/src/settings.rs`).

### The bounds, as built (2026-10-30)

Phase 1 of cassini-backpressure.md with its decisions, on top of the numbered publishes and acknowledged deliveries
(cassini-client-reliability.md, steps 2 and 5). Paths relative to `src/agents/cassini/`.

- **The frame limit:** 64 MiB (`MAX_FRAME_BYTES`, `types/src/lib.rs`). The Listener's reader refuses a larger header
  and ends the connection before allocating, logging the identity; the client refuses to send one
  (`TcpClientActor::frame`, and `TcpClient::publish` returns `TooLarge` for a payload over `MAX_PAYLOAD_BYTES`). The
  accept loop waits 100 ms after a failed accept (EMFILE) instead of spinning.
- **One budget** (`broker/src/budget.rs`, `Budget`): every payload byte the broker holds. A reader charges a frame
  before it goes on (`Budget::try_charge`, `charge`); the charge travels with the payload (`cassini_types::Payload`,
  `types/src/held.rs:80`), which every hop shares, and is refunded when its last holder drops it, so a message fanned
  out to many subscribers is counted once. The budget is accounting only: what it holds, its alarm and its counters;
  no decision across actors is made through it.
- **Its limit is set at run time** (cassini-backpressure.md, "Runtime budget"; `broker/src/runtime_budget.rs`): from a
  floor of four frames, which memory must give at start or the broker does not start, up to a ceiling sampled every
  second from the host's `MemAvailable`, the container's `memory.max` and, when the kernel shows them, PSI and
  `memory.events`, toward a target of the drain rate times the stall bound; `CASSINI_MEMORY_BUDGET_BYTES` caps it
  (`settings.rs`). A limit set below what is held evicts nothing: the alarm stops admission until it drains
  (`Budget::set_limit`); memory below the floor at run time stops admission until it returns (`set_starved`). A test's
  broker, built from `Bounds` directly, keeps a fixed budget.
- **Exhausted** when a frame cannot be charged: from then every reader waits (a memory alarm), until what is held
  falls to three quarters of the budget. The pressure manager says when it rises (with the five largest holders by
  name: subscribers as `<registration id>:<topic>`, topics as `topic <name>`) and when it lifts.
- **Ingress credit per connection** (`Window`, `budget.rs:364`): a quarter of the budget, never less than one frame.
  A frame over 64 KiB (`SMALL_FRAME`) is admitted, window then budget, before its body is read; a smaller one is read
  first, so acknowledgements and subscriptions are never stuck behind the frames they would make room for, and a
  publish among them is admitted before it goes on (`listener.rs`, `admit` at 128, the read loop from 1116, `charged`
  at 351). While a small publish waits the reader goes on reading: an acknowledgement is passed on at once, and the
  first other frame is kept (only its length, if it is a large one) until the publish is admitted, when reading
  stops and TCP pushes back (`Next`, `read_header`, `read_body`, `listener.rs:178-272`). The client writes its delivery
  acknowledgements from a task of its own per connection, beside the transport's mailbox, so they never wait behind its
  publishes there (`spawn_ack_writer`, `client/src/lib.rs:613`). The credit travels in the publish
  (`BrokerMessage::PublishRequest { window }`) and is released when the topic accepts it.
- **Per-subscriber high water** (`cassini_types::Backlog`, `types/src/held.rs`): the bytes in the subscriber's
  delivery outbox, delivered and not acknowledged, counted as their frames were charged (`Payload::weight`); the mark is
  an eighth of the budget as it is now (never under 1 MiB; each subscriber follows it at each stall check), with
  hysteresis down to half of it. A topic accepts no publish (no fan-out, no acknowledgement) while a subscriber of it
  says it holds the topic back (`TopicHold`: connected and above its mark; told to the ref the topic gave it,
  `TopicAttached`): the publish waits in the topic, in order, holding its connection's credit (`topic.rs`, `pump`,
  `accept`). The topic reads no subscriber's state.
- **The pressure manager** (`broker/src/pressure.rs`), the broker's linked child: at each stall check (a quarter of the
  stall bound, between 50 ms and 5 s) it tells the subscriber manager, which tells each subscriber, `BudgetExhausted`
  while the budget is exhausted and `StallCheck` otherwise; every second it samples memory and sets the budget's
  limit. It ends with the drain; one that ends otherwise is started again by the broker. It answers a typed snapshot
  (`PressureMsg::Snapshot`) and takes a warm start (`PressureMsg::WarmStart`).
- **The stall bound** (`CASSINI_STALL_SECS`, default 30 s, more than 0 and under 60 s): a subscriber looks at its own
  backlog (`look_for_stall`, `subscriber.rs`), and has stalled when it is above its high water for the bound, or, while
  the budget is exhausted, holds anything with nothing acknowledged for the bound. The clock counts only time spent
  connected: a subscriber that went above its mark while away starts it when it comes back (`Backlog::set_connected`).
  It tells its session (`SubscriberStalled`), which decides by the one stall policy (`pressure::verdict`) from its own
  facts: what its connection's reader told it, beside the Listener's mailbox (`ConnectionProgress` when it reads an
  acknowledgement, `ConnectionWaiting` when it waits for an admission). A connection spared less than the bound ago is
  not judged again; while the budget is exhausted, an acknowledgement on any topic within the bound is progress; a
  connection whose reader waits for room is spared (its reader refuses for now, and reads on), again only once it has
  made progress since; otherwise it is condemned, and a later look (an eighth of the bound on) that still finds so cuts
  it, from outside its Listener (which may be waiting on a write to
  that client) through the control the Listener gave it (`ConnectionOpened`, a weak handle), and the session manager
  logs it with the identity, the topic, the bytes held and for how long. The session detaches for `R` as on any lost
  connection, and the topic moves again. Recovery from an exhausted budget is so bounded by the stall bound.
- **Refused for now** (`admit_or_refuse`, `listener.rs:391`): a spared connection's reader refuses each publish it
  cannot admit at once (`PublishRefusal::Unavailable`, `REFUSED_FOR_NOW`), and reads on; a large one's body is read
  past under one global slot within the stall bound and let go (`REFUSED_BODY`), so the budget is passed by at most
  one frame while connections refuse. A facade keeps what is refused and sends it again, its wait doubling from 1 s to
  30 s, one resend at a time (`client/src/facade.rs`, `Deferral`); a bare transport tells its caller to
  (`DEFERRED_NOT_KEPT`).
- **Frames are numbered** on each connection, from one, by the Listener's writes and the client's reader: a client that
  cannot decode a frame names it (`ClientMessage::FrameUndecodable`), and the delivery it carried is let go as
  acknowledged (`listener.rs:1681`).
- **Holders bounded by the budget:** while it is exhausted, a topic's queue for no subscribers refuses the newest,
  answering its publisher with a refusal for now (`PublishRefusal::Unavailable`), which its client keeps and sends
  again, not an acknowledgement; a subscriber whose client is away refuses the newest, counted and logged; and at each
  stall check every subscriber whose client is away lets go of all it held (`shed_if_away`), counted and logged. Its
  session then ends at once, not at `R` (`EndSession`), logged with the identity: a client that comes back asking for
  it is told, with why (`ClientMessage::SessionEnded`), before it is given a new session. The age bounds (`R`, the
  queue's 180 s) stay.
- **A protocol version** at registration (`cassini_types::PROTOCOL_VERSION`): a registration of another version is
  refused by name (`RefusalCode::ProtocolMismatch`), and a first frame that does not decode (an older client's
  registration) with a refusal such a client can decode; either connection is closed.
- **Observable:** the budget's report every 30 s while something is held, waits or was counted, from the pressure
  manager (`PressureMsg::Report`, `broker/src/budget_report.rs`: use, peak, readers waiting, refusals, slow-subscriber disconnections, alarms, the
  largest holders), and the log lines above. There is no metrics path in the broker to add them to: `metrics.rs` is
  not a module and `ControlOp::GetBrokerStats` is not implemented.
- **Also changed:** the Listener's read loop and its delivery write no longer hold an entered span across a wait
  (the trap recorded under "Policy in the broker").
- **Tests:** `budget.rs`'s and `types/src/held.rs`'s own; `broker/tests/props.rs`,
  `the_budget_holds_each_message_once_and_never_more_than_its_limit` (the budget as a model);
  `broker/tests/backpressure.rs` (a subscriber that stops reading, with 256 KiB and with 16 KiB messages, under a
  budget its alarm never reaches: its backlog goes above the high water and holds the topic back, the publisher's
  acknowledgements stop, the connection is ended after the stall bound, the publisher completes, the session is kept;
  a slow subscriber within the bound receives everything in order; one back from away is given the bound to catch
  up; the frame-byte count; the recovery from an exhausted budget, with acknowledgements read around a waiting
  publish, a processor that publishes to itself, subscribers under their mark that stop reading, and a subscriber
  away that lets go of all it held; a processor not cut under an exhausted budget, with small and with large frames,
  or holding back its own topic; one that never acknowledges spared once, then ended; progress per connection; a
  delivery the client cannot decode; a budget under a frame is refused); `settings.rs`'s; `broker/tests/frames.rs`.
- **Known limits:** a large frame's body is not read before it is admitted, so an acknowledgement behind it waits,
  as does any frame behind the one kept while a small publish waits, until its connection is spared and refuses for
  now. A client that never reads blocks its own Listener on a write, so its publishes never reach a topic: it holds up
  to its window and high water until the budget is exhausted and it is ended. The broker's own working memory (the
  copy a write makes of a frame, TLS buffers, mailboxes of small messages) is outside the budget, and the runtime
  ceiling counts it (`memory.current − held`) only where the container has a limit; per-message overhead is not charged
  (cassini-backpressure.md, review finding 6). High water is soft: a subscriber tells its topic it holds it back by a
  message, and a topic resuming pumps what waits before it hears again, so a subscriber can pass its mark by the
  connections' windows; the budget still bounds the whole (finding 7).

## Running it on its own

The broker needs no other part of Polar to start and route: three PEM files from any CA and a shutdown token. With no
credentials agent there is no policy, so each client gets its certificate's floor and nothing more; the steps, checked
2026-10-28, and the ways past the floor are in `src/agents/cassini/broker/readme.md`, "Running it on its own".

## Testing

`broker/tests/support/mod.rs` runs the real broker on loopback with a throwaway CA (`Pki`, 54; client certificates
with `p:` and `g:` URI SANs from `issue_client_carrying`, 208), `TestBroker` (238) and `TestClient` (634), which collects
`ClientEvent`s and the broker's refusals (`refusals`, 884). Most tests start the broker with an open policy put straight
into its policy actor (`OPEN_POLICY`, 230; `set_policy`, 477), which the binary has no way to do; `start_enforcing`
(336) starts it with none, as a broker without the credentials agent runs, and `tests/enforcement.rs` plays the
credentials agent with a signed snapshot. *(Updated 2026-10-28.)*
