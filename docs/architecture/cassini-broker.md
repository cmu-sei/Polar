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

The handshake is mutual TLS (`WebPkiClientVerifier`, `listener.rs:183-197`). Right after it, before the stream is split,
`extract_peer_identity` takes the leaf's **first DNS SAN** (`listener.rs:89-116, 283`). It fails open: no SAN gives
`None`. The certificate itself is then dropped, so its `p:` and `c:` URI SANs are never seen past that point. To read
them, the leaf has to be kept at that line and carried in `ListenerArguments`/`ListenerState` (595-615); the extractors
exist (`cert-issuer/common/src/permission.rs:185`, `capability.rs:134`), and `PolicyMessage::CheckAuthorization` takes the
certificate as PEM.

| Where | Has the client's identity |
|---|---|
| Listener | yes, `state.peer_identity`, in `handle_subscribe_request` (1279) and `handle_publish_request` (1119); unused |
| SessionManager | as `Session.identity` (56); a SessionAgent's state has none (781-786) |
| Broker, TopicManager, SubscriberManager | no: a subscribe carries the registration id and topic; a publish carries `published_by` only when the publisher shares it |

`published_by` is the verified identity, stamped by the broker on a publish when the client sets `share_identity` (the
default) (`types/src/lib.rs:367-405`). Receivers check it where it matters (`AuthorityCore`'s publisher check).

**A flaw:** resuming a session with an existing registration id does not compare the new connection's identity with the
session's (`session.rs:595-622`), so a registration id works as a bearer token. A decision must use the connection's
identity (the Listener's), not the session's.

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
- **Watcher sessions (2026-10-27).** Beside an identity's session, a watcher of it may run: below, "Watcher
  sessions".
- **Revoked certificates (2026-10-27, `key-custody.md`, step 5).** The listener reads the leaf's serial at accept. A
  certificate the credentials agent's revoked set names is refused at registration, unless it registers a watcher, and
  on each change of authority every connection whose certificate is now revoked is closed, a watcher's apart; the
  session is kept for the retention window, for the identity's next certificate to claim. Tests:
  `broker/tests/revocation.rs`, `custody_revocation.rs`.
- **Why:** a replaced pod (a rollout, a Job retry) usually registers before the broker sees the old connection is dead.
  The replacement takes over and inherits what was waiting for the old one.
- **Fan-out.** A topic delivers every message to every subscriber (`topic.rs:728`). There are no consumer groups.
- **So a workload cannot scale out.** The identity is the workload's (the certificate's first DNS SAN), so its pods
  supersede each other. Identities per pod would stop that but would not share the work: with fan-out, every replica
  receives every message (two `kube-consumer`s would both write each to the graph). Scaling a consumer or processor out
  needs both a way to tell a replica from a replacement and delivery shared within a group; an observer that scales out
  has to split its source, which the broker cannot do for it. Open, in `remaining-work.md`.

## Watcher sessions (2026-10-27, `key-custody.md`, step 5)

A watcher is a second, limited session under an identity's certificate, beside the identity's own. Only the custody
daemon uses one: it holds the pod's key, connects as the pod (the pod's certificate is the only one it has), and must
hear of its own revocation whatever the workload does. Without a kind of its own, its session and the workload's would
supersede each other in turn.

- *Declared, not proven.* A client asks for one at registration (`ClientMessage::RegistrationRequest { watcher }`;
  `TCPClientConfig::as_watcher`). The broker cannot tell the daemon's connection from the workload's, both presenting
  the pod's certificate, so the rules make declaring one gain nothing.
- *The rules* (`session.rs`, `listener.rs`):
  - it neither supersedes nor is superseded by its identity's session; a new pod's session still supersedes the old
    pod's;
  - it is never resumed and resumes nothing: it ends when its connection goes, nothing held for it, and registers afresh
    each time;
  - it claims nothing held for its identity's superseded sessions (`GetHeldSessions` answers a watcher nothing): that
    is the identity's next session's;
  - it may subscribe to `polar-authority-updates` and `polar-authority-snapshot-replies` and publish to
    `polar-authority-snapshot-requests`, nothing else (`authority::watcher_may`, `WATCHER_REFUSAL`), and within those it
    is decided as its identity's session is (Cedar, then the certificate's floor), so a watcher may never more than its
    identity;
  - a revoked certificate may still register one, and on a change of authority the broker leaves watchers open, so the
    daemon is never cut off before it hears its own revocation.
- *What a holder of the certificate gains from one:* the authority topics, signed and not secret, which every agent
  hears anyway, until the certificate ends (an hour for a client's). With the daemon nobody holds the key once it is
  revoked; with key files a stolen key could listen that long.
- *Its weak points:*
  - nothing bounds how many watchers an identity opens: a compromised workload could open hundreds, each holding the
    broker's memory and a connection and each able to ask for snapshots (the plan below);
  - it proves nothing about who opened it; nothing needs that today;
  - the broker's own pod has no client slot, so its daemon has no watcher, and the broker's server key ends only with
    its certificate (12 hours on the local targets);
  - a revoked watcher is left open, not ended: the daemon exits once it has heard, but a stolen key file's watcher would
    not.
- *Tests:* `broker/tests/watcher.rs`, `revocation.rs`, `custody_revocation.rs`.

### Capping watchers: the plan (2026-10-27, on the to-do list)

1. *One watcher per certificate, the newest winning.* A watcher registering under the same identity and certificate
   serial as a watcher already there ends the older: its connection closed and its session ended, nothing held.
   - *Per serial, not per identity.* Each pod's certificate is its own (a new serial for each pod and each restart), so
     one per serial bounds each holder of a certificate to one watcher, and a replacement pod's daemon never displaces
     the old pod's while both run in a rollout.
   - *Newest wins, not first wins.* With first wins a compromised workload could take the slot first and keep its daemon
     out for good, and the daemon would never hear its revocation. With newest wins the daemon, which reconnects each
     tick and asks for a snapshot each time it registers, always gets back in: at worst the two take turns, which gains
     the workload nothing, and each turn's request is coalesced by the credentials agent (five seconds).
   - *What it bounds:* watchers of an identity to its live certificates, and snapshot deliveries to two for each pod.
2. *The serial reaches the session manager with the registration,* read by the broker from the handshake beside the
   identity (`BrokerMessage::RegistrationRequest`), never sent by the client.
3. *Tests first:* two watchers under one certificate, the older closed and its session gone; two under different
   certificates of one identity, both kept; the identity's own session untouched throughout; a property, that after any
   sequence of registrations and disconnections at most one watcher is live for each serial and it is the newest; end
   to end, a second watcher under the pod's certificate opened against the daemon's, and the daemon still hearing its
   revocation. Mutations on each rule.

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
  decision runs with the span left and instrumented by it. The Listener's writes still wait inside entered spans, which
  has not been seen to fail; it would under a slow reader, when a write waits on the socket.
- **Tests:** `broker/tests/enforcement.rs` (declared admitted and undeclared refused with no policy; implied topics;
  a certificate naming nothing; a Cedar forbid and permit; a snapshot believed from the credentials agent and not from
  an impostor allowed to publish on the topic). Every other broker test starts its broker with a policy that permits
  everything (`support::OPEN_POLICY`); `TestBroker::start_enforcing` starts one without.

Before stage 5b: the broker is built with `ServiceBuilder` (`CASSINI_CLIENT = false`, `broker/src/agent.rs:44-53`), so it has a health
check but no `AuthorityHandle`: no PolicyActor, no PermissionsActor, no authority updates
(`polar-agent-builder/src/infrastructure.rs:614-633`). It cannot be a client of itself (its certificate is a server
certificate). `AuthorityHandle::spawn` is public, so the broker can start one itself and feed it the authority topics'
publishes in process, with the publisher's identity from its own Listener.

## Testing

`broker/tests/support/mod.rs` runs the real broker on loopback with a throwaway CA (`Pki`, 54-108; one DNS SAN per client
certificate; a URI SAN variant would follow `permission.rs:531-538`), `TestBroker` (163-222) and `TestClient` (412-595),
which collects `ClientEvent`s. A refused subscribe or publish is invisible to it until the client turns refusals into
events. `shutdown.rs:151` is the model for a "refused, and told so" test.
