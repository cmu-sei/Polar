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

# Backpressure for the Cassini broker and client

Status: **cleared and built 2026-10-30** ("As built", below; the decisions are at the end). Not built: the
domain-deferred acknowledgement, the offline file and deduplication. What the broker and client bound today is in cassini-broker.md ("Bounds
and flow control") and cassini-client-reliability.md ("As built, 2026-10-30"); in short, nothing, except the session
retention `R`. The measurement that started this: at 64 KiB saturated, a sink that drained about 11% slower than the
broker accepted left 46 to 176 MB of payload in the broker's heap within a second (broker-baseline.md, the fourth set).
The broker is installed on machines with little memory, where less memory under load is a stability gain
(broker-baseline.md, the maintainer, 2026-10-29).

## What it must do

1. **Bound the broker's memory** by a budget it knows, whatever its clients do.
2. **Push the pressure back to the publisher** that causes it, over TCP, instead of holding it in the broker.
3. **Bound the client's memory too:** today a broker that stops reading moves the backlog into the agent (the
   transport's unbounded mailbox) and `publish` keeps saying `Ok`.
4. **Keep one slow subscriber from stalling everything for long:** a bound on how long it may hold a topic back, then a
   rule.
5. **Lose nothing silently:** whatever is refused or dropped is counted, logged and, where a caller can act, returned.
6. **Fit the reliability design:** its outbox and per-message acknowledgements are the credit a later phase uses.

## Phase 1: the broker bounds itself and tells TCP (no wire change)

- **One budget** for every payload byte the broker holds (in flight from a reader, queued for a subscriber, in a topic's
  queue, in a dead-letter queue). A payload carries its charge: the broker wraps it once on admission, every hop shares
  the `Arc`, and the last holder to drop it refunds the budget, so a message fanned out to many subscribers is counted
  once and nothing can forget to refund. Default: a share of the container's memory limit (`memory.max` of its cgroup),
  overridable by a broker setting.
- **A frame limit:** the reader takes the 4-byte length, refuses a frame over the limit (closes the connection, logs
  the identity) without allocating it, and only then reads. The client refuses to send one (an error to the caller).
- **Ingress credit per connection:** before reading a frame's payload the reader takes its bytes from the connection's
  window and from the budget; while it cannot, it does not read, and TCP's window does the rest. The window is released
  when the topic acknowledges the publish; the budget charge stays with the payload.
- **Per-subscriber high water:** each subscriber counts the bytes cast to it and not yet written to its socket. A topic
  does not accept a publish while any of its subscribers is above its high-water mark; the publish waits in the topic
  (bounded by the publisher's window), unacknowledged, so the publisher's window fills and its connection stops being
  read. Lossless, at the price of head-of-line blocking on that topic.
- **The stall bound:** a subscriber above high water for longer than `T_stall` is slow. The rule is decision 1 below;
  the recommendation is to end that subscriber's connection (as NATS does with a slow consumer): its session detaches
  for `R` as any disconnect does, what it was owed stays only while the budget allows, and the topic moves again.
  `T_stall` must be under `TCP_USER_TIMEOUT` (60 s, `types/src/socket.rs`), which may already cut a peer that shows a
  zero window that long (to verify).
- **The holders get the budget's bound:** the topic's no-subscriber queue and each dead-letter queue charge the budget;
  when it is exhausted they refuse the newest (counted) rather than grow. Age bounds (`R`, 180 s) stay.
- **An exhausted budget stops every reader** (a memory alarm): logged once with the largest holders by topic and
  subscriber, and lifted when the budget falls below a low-water mark. Deliveries keep draining, so it recovers.
- **Observable:** the budget's use, each subscriber's backlog and high-water time, readers waiting, slow-subscriber
  disconnections and refusals, as log lines and in the broker's existing metrics path.

## Phase 1, client side

- **The facade bounds what it has handed the transport and not yet written** (messages and bytes; the transport reports
  each write). When that is full the caller's `OfflineBehavior` decides, as when offline: `Queue` holds it in the
  facade's buffer, `Fail` returns `Busy`, `Drop` discards it and counts it.
- **The buffer stops dropping silently:** when it is full it refuses the NEWEST and says so (an error for `Queue`
  callers, and a `ClientEvent`), instead of discarding the oldest with only a warning.
- **An async way to wait:** `publish_when_ready(..).await` waits for room, for callers that can await (jira-observer,
  kube-observer), so pressure slows them instead of failing them. The synchronous `publish` stays for callers that
  cannot await.
- **The call sites that would break** (cassini-client-reliability.md, "As built"): jira-observer and kube-observer move
  to the awaiting form; git-repo-observer collects a revwalk's events and publishes them after the walk, and no longer
  advances `last_seen` past an event that was not accepted (today it loses commits silently, pressure or not).

## Phase 2: credit from acknowledgements (with reliability step 2)

Once messages carry an id and every publish is acknowledged by it (cassini-client-reliability.md, steps 2 and 5), the
client's outbox of unacknowledged messages is the credit window, and a subscriber's delivery outbox (held until its
`PublishResponseAck`) replaces "written to the socket" as the per-subscriber measure. A consumer that acknowledges after
its graph write then slows the publisher by how fast it actually processes. Phase 1's budget, frame limit and stall
rule stay.

## Tests, first

- **The budget as a model (property test):** for any interleaving of admissions, fan-outs, writes, expiries and
  disconnects, the bytes held never exceed the budget and are all refunded at the end; for any message, its charge is
  taken once and refunded once.
- **A subscriber that stops reading** (a `TestClient` that subscribes, then keeps its socket open and reads nothing):
  the broker's budget use stays under its bound, the publisher's acknowledgements stop, after `T_stall` that subscriber
  is disconnected and the publisher resumes; a slow subscriber that stays within `T_stall` receives everything, in order.
- **An oversized frame** closes the connection without the allocation (a crafted header).
- **The facade:** the bound, each `OfflineBehavior` under pressure, `publish_when_ready` waking when room returns, and a
  full buffer refusing the newest with an event.
- **git-repo-observer:** an event refused by the client leaves `last_seen` where it was.
- **The harness:** a sink that can be slowed (a delay per message), the broker's memory in every record (as on
  `feat/musl-mimalloc`), and a plan `slow-sink.nuon`; acceptance: the broker's peak stays within its budget plus a fixed
  overhead, and nothing is lost below `T_stall`.

## Decided (the maintainer, 2026-10-30)

1. **A subscriber that stays slow past `T_stall` has its connection ended.** Nothing is lost before then; after, its
   session detaches for `R` as on any disconnect, it is owed what the budget still holds, and the topic moves again.
2. **The budget comes from the container's memory limit:** a share of its cgroup's `memory.max`, overridable by a broker
   setting; the per-connection window and the per-subscriber high water are fractions of it.
3. **The frame limit is 64 MiB.** Consequences: a connection's window is never below one frame (64 MiB), and a budget
   under 64 MiB could never admit a full frame, so a broker whose budget is smaller refuses to start and says why. The
   client refuses to send a larger frame, with an error to the caller.
4. **Both phases together:** reliability step 2 (messages carry `(epoch, seq)`, `PublishRequestAck` returns them, the
   client's outbox of unacknowledged messages) and the acknowledged deliveries of step 5 (`PublishResponse` carries a
   delivery id, `PublishResponseAck` returns it, the broker keeps a subscriber's unacknowledged deliveries), with the
   bounds on top. The credit is the outbox on the client and the delivery outbox per subscriber; the budget, frame limit
   and stall rule are as in phase 1. Not in this build: the offline file (step 3) and deduplication (step 4); delivery
   stays at-least-once across a reconnect, which the handlers' MERGE-style writes tolerate (reliability doc, 5a). The
   wire format changes, so the broker and every agent move together (one artifact per target).

## As built (2026-10-30)

Both phases together, as decided. Paths relative to `src/agents/`. Commits in order: the frame cap (cherry-picked,
its author's), its tests and the client's refusal; the numbered publishes and the outbox; the acknowledged deliveries;
the broker's bounds; the client's bounds; the harness's slow sink.

- **The frame limit, 64 MiB:** cassini-broker.md, "The bounds, as built" (and `cassini/types/src/lib.rs`,
  `MAX_FRAME_BYTES`, `MAX_PAYLOAD_BYTES`).
- **The credit on the wire:** the client's outbox of unacknowledged messages and per-message acknowledgements
  (cassini-client-reliability.md, "Step 2, as built"); the subscriber's delivery outbox and `PublishResponseAck`
  ("Step 5's acknowledged deliveries, as built").
- **The broker's bounds** (budget, window, high water, stall rule, holders, alarm, report): cassini-broker.md, "The
  bounds, as built". The per-subscriber measure is the delivery outbox, delivered and not acknowledged (phase 2's,
  not phase 1's "not yet written").
- **The client's bounds** (`cassini/client/src/credit.rs`): the outbox is the credit, 1,000 messages or 64 MiB
  (`outbox::DEFAULT_OUTBOX_LIMITS`), and the buffer behind it 1,000 or 64 MiB (`DEFAULT_BUFFER_LIMITS`, `credit.rs:40`).
  A publish takes its room before it is handed to the facade (`Credit::take`, `credit.rs:118`): in the outbox when
  there is room and nothing waits in the buffer; otherwise its `OfflineBehavior` decides. `Queue` takes room in the
  buffer, and when that is full the newest is refused, with `CassiniClientError::BufferFull` to the caller and
  `ClientEvent::PublishDropped` to the agent (no more silent drop-oldest); `Fail` is refused with
  `CassiniClientError::Busy`; `Drop` is discarded and counted (`Credit::dropped`). `TcpClient::publish` decides so
  synchronously (`lib/src/cassini.rs:382`); `TcpClient::publish_when_ready(..).await` waits for room in the outbox
  (`lib/src/cassini.rs:364`, `Credit::wait_for_outbox`). Offline, `Fail` and `Drop` answer as before. A `Publish`
  cast to the facade by an agent that holds its address takes room there, as `Queue` (`facade.rs`, `admit` 290,
  `refuse_newest` 335); the facade gives room back as the broker acknowledges or refuses, and moves what waits in the
  buffer into the room made (`promote` 348).
- **The call sites** (cassini-client-reliability.md, "As built"): jira-observer's `publish_data` and kube-observer's
  `emit_event` await `publish_when_ready` (`jira/observe/src/lib.rs:485`, `kubernetes/observe/src/lib.rs:329`);
  git-repo-observer's walk only collects (`observe_refs`, `git/observe/src/lib.rs:835`), and what it found is
  published after it, a ref at a time, moving that ref's `last_seen` only once all its events were accepted
  (`publish_observed`, `git/observe/src/lib.rs:537`, through `git_agent_common::publish_event_when_ready`). The
  gitlab observer and the other agents that hold a `TcpClientActor` itself (the CLI daemon, cassini-c2, the custody
  daemon's watcher, openapi, provenance's `polar::lib`) need no change: the transport numbers what it is given in an
  epoch of its own and acknowledges deliveries itself; they have no outbox (nothing is resent) and no credit. The
  scheduler pair, which casts to the facade's address, is bounded as a `Queue` caller and told of a refusal by
  `PublishDropped`.
- **Tests:** `credit.rs`'s own; the facade's (`the_buffer_is_bounded_and_refuses_the_newest_and_says_so`, which
  replaces the test that asserted the oldest was dropped; a publish kept where it took room and its room given back);
  `lib/src/cassini.rs`'s `at_the_bound_each_offline_behavior_decides` and
  `publish_when_ready_waits_for_room_and_goes_when_the_broker_acknowledges`; git-repo-observer's
  `an_event_the_client_refused_leaves_its_ref_s_last_seen_where_it_was`.
- **Not done:** the domain-deferred acknowledgement (a consumer acknowledging after its graph write; the hand-off
  is acknowledged, `TcpClientActor::delivery_handed_off`, the one call to move); the offline file (reliability step 3:
  a facade that ends loses its outbox and buffer); deduplication (step 4: a message resent after a lost
  acknowledgement, or a delivery resent on resume, arrives twice); an outbox entry older than `R` is not dropped
  (5a). The harness's slow sink slows its handling, not its acknowledgements, so until the acknowledgement is the
  domain's its backlog waits in the sink, not the broker; the broker side of a slow reader is what
  `cassini/broker/tests/backpressure.rs` tests.

The choices the decisions settled are kept below for the record.

## Review before merge (2026-10-30): defects found, then fixed on the branch

Read from the code of `79980d3b..d70bbfb7`; paths relative to `src/agents/`. Confirmed means read end to end (three
also checked by hand); plausible means each link was read but the whole was not reproduced.

1. **An exhausted budget can stop the broker for good (plausible).** A reader waiting in `admit` for a publish
   (`listener.rs:1071, 1129`) reads nothing behind it, acknowledgements included, and the client writes its delivery
   acks from the same mailbox as its publishes (`client/src/lib.rs:1730`). A client that both publishes and subscribes
   (a processor) then cannot refund what its delivery outbox holds. The stall rule does not rescue it: high water
   counts payload bytes (`subscriber.rs:952`) where the budget charges frame bytes, and `BudgetExhausted` is sent only
   when the alarm is raised (`budget.rs:264`). The claim at `budget.rs:64-67` holds only when the head frame is not a
   publish.
2. **A resumed subscriber is cut again at once (confirmed).** `Backlog::add` starts `above_since_ms` while the
   subscriber is away (`types/src/held.rs:198-201`); `set_connected` (`held.rs:241`) does not reset it, so a subscriber
   away longer than `T_stall` is ended by the next `StallCheck` after it resumes, replays, and loops.
3. **The client's credit can wedge (confirmed).** `TcpClient::publish` takes a `Buffer` slot, then casts; if the acks
   empty the outbox before the cast lands, `keep` pushes it onto the buffer (`facade.rs:324`) without `promote`, it is
   never sent, and every later `publish_when_ready` waits for ever.
4. **A publish dropped for want of budget is still acknowledged (confirmed)** (`topic.rs:820-846`), against design
   goal 5.
5. **Transient refusals settle the outbox (confirmed).** Any `Err` with an origin becomes `Refused`
   (`client/src/lib.rs:900`, `facade.rs:507`), including "Broker is shutting down", "forward failed" and "topic manager
   unavailable", so a rolling restart loses what the outbox exists to resend. Only a policy refusal should settle.
6. Per-message overhead is not charged (`Pending`, outbox entries, `Listener.deliveries` at `listener.rs:1710`, cleaned
   only by acks); with small messages real memory can be a multiple of the budget (plausible).
7. High water is soft: `pump` on `TopicResume` overshoots it by the waiting windows; the budget still bounds it.
8. git-repo-observer collects a whole walk before publishing (`git/observe/src/lib.rs` ~748); a first walk of a large
   repository is held in memory at once (plausible).

Wire: `PublishResponseAck` was inserted mid-`ClientMessage` (`types/src/lib.rs:748`), shifting later tags; an old
client's frames mostly fail `rkyv::access` and are dropped without closing the connection, so it hangs rather than
being told. All images are rebuilt together; a version check at registration would name the mismatch.

Tests that cannot fail: `stats.peak <= BUDGET` in `broker/tests/backpressure.rs` (`try_charge` never exceeds by
construction), and its `stopped_at < count` passes on the global alarm alone. None covers findings 1 to 3.

Also: the tracing panic ("tried to clone a span ... already closed") is `span.enter()` guards held across `.await` in
the broker and client; the assert is not debug-only (tracing-subscriber 0.3.23 `registry/sharded.rs:317`), so it panics
a worker in release. On this branch `revocation` failed 3 runs in 3, on `main` 1 in 3 (run alone, 2026-10-30).

### Fixed (2026-10-30)

The branch was rebased onto main's tracing fix (`70d62aec`, which holds no entered span across an await); the read
loop it conflicted with keeps both: no guard across a wait, and the frame check and admission. Since, `revocation` and
`custody_revocation` pass 5 runs in 5 each, run alone. Each fix below came
with a test that failed first, and fails again with the fix mutated away. Paths relative to `src/agents/cassini/`.

1. **Recovery from an exhausted budget is bounded by the stall bound.** Acknowledgements never wait behind a
   publish's admission: the client writes them from a task of its own per connection, beside its mailbox
   (`spawn_ack_writer`, `client/src/lib.rs:613`), and the broker's reader, while a small publish waits, reads on,
   passing acknowledgements at once and keeping the first other frame until the publish is admitted (`Next`,
   `broker/src/listener.rs:178`, the loop from 1146). High water counts frame bytes, as the budget charges them
   (`Payload::weight`, `types/src/held.rs:105`, set from the charge in `charged`); an away subscriber that sheds lets
   go of all it was counted for (`shed_if_away`, `subscriber.rs:852`). The budget's watchdog (`spawn_watchdog`,
   `budget.rs:389`) runs every stall check while the budget is exhausted: it tells the Listeners again (the alarm no
   longer reaches only those away when it rose), and ends the connections of subscribers that hold anything and have
   made no progress for the stall bound (`Backlog::stalled_for`, `held.rs:217`), so subscribers each under their
   mark cannot hold the budget for good. Tests (`broker/tests/backpressure.rs`):
   `subscribers_below_their_high_water_that_stop_reading_do_not_stop_the_broker` (before: still exhausted after six
   stall bounds; it also fails with the alarm not re-sent, and with acknowledgements through the mailbox, when the
   processor is taken for stalled), `acknowledgements_behind_a_publish_that_waits_for_the_budget_are_read` (before:
   the publish waited out the stall bound), `what_a_subscriber_is_owed_counts_each_message_s_frame_not_only_its_payload`,
   `a_subscriber_away_when_the_budget_is_exhausted_lets_go_of_all_it_was_owed`,
   `a_client_that_publishes_and_subscribes_under_an_exhausted_budget_recovers`, and `held.rs`'s
   `a_subscriber_stalls_from_its_last_progress_while_it_holds_anything`.
2. **A resumed subscriber is given the stall bound.** The stall clock runs only while the subscriber is connected
   above its mark: `add` starts it only when connected, and a return above the mark starts it afresh
   (`Backlog::add`, `set_connected`, `held.rs:239, 293`). Tests: `held.rs`'s
   `the_stall_clock_runs_only_while_the_subscriber_is_connected`; `backpressure.rs`'s
   `a_subscriber_back_from_away_is_given_the_stall_bound_to_catch_up` (away past the bound with 16 MiB held; before:
   cut at once).
3. **The client's credit does not wedge.** A publish kept in the buffer after the outbox emptied is promoted at once
   (`keep`, `client/src/facade.rs:333`). Test: `facade.rs`'s
   `a_buffered_publish_that_arrives_after_the_outbox_emptied_is_sent` (before: never sent).
4. **A publish dropped for want of budget is not acknowledged.** The topic answers it with a refusal for now
   (`topic.rs:837`), which its client keeps and sends again. Test: `broker/tests/topic.rs`'s
   `a_message_no_one_can_hold_while_the_budget_is_exhausted_is_refused_for_now`.
5. **A transient refusal does not settle the outbox.** The refusal is typed on the wire:
   `PublishRefusal { Denied, Unavailable }` (`types/src/lib.rs:636`), in `PublishResponse { refusal }`. The broker
   marks policy refusals `Denied` and the transient ones `Unavailable` (shutting down, forward failed, no broker, the
   budget: `broker.rs:909`, `session.rs:1235, 1271`, `listener.rs:1807`, `topic.rs:868`); the client settles only a
   `Denied` one, and defers an `Unavailable` one (`ClientEvent::PublishDeferred`, `client/src/lib.rs:956`): the facade
   keeps it in the outbox and sends it again after its backoff, or on the next registration (`Deferred`, `Resend`,
   `facade.rs:520`). Tests: `client/src/lib.rs`'s `an_unavailable_refusal_defers_the_publish_and_a_denied_one_settles_it`;
   `facade.rs`'s `a_deferred_publish_is_kept_and_sent_again_and_a_refused_one_is_not`; `broker/tests/acknowledgments.rs`'s
   `a_publish_refused_while_the_broker_shuts_down_is_sent_to_the_next_broker`.

The tests that could not fail are repaired: `peak <= BUDGET` is gone, and `stops_reading` runs under a budget of
256 MiB, which one subscriber cannot exhaust: it asserts the alarm never rose, and that the subscriber's backlog went
above its high water and held the topic back.

**Known limits, not fixed (findings 6 to 8):** per-message overhead outside the payload is not charged (the
`Pending`, outbox entries, `Listener.deliveries`), so with small messages real memory can exceed the budget; high
water counts frames now, which narrows it for what subscribers hold, not for the broker's bookkeeping. High water is soft:
`pump` on `TopicResume` can pass the mark by the waiting windows; the budget still bounds the whole.
git-repo-observer holds a whole walk before publishing it, so a first walk of a large repository is in memory at
once. And a large frame's body is not read before it is admitted, so an acknowledgement behind it waits, as does any
frame behind the one a waiting small publish keeps; the watchdog bounds that by the stall bound.

**Not done:** a protocol version at registration (the wire note above). Every image is still rebuilt together; a
mismatched client still fails to decode rather than being told.

## Second review (2026-10-30): the watchdog cut healthy clients, fixed on the branch

A second review of the branch (rebased onto main `98b1effb`) found that the watchdog of the first review's fix 1
ended clients that read and acknowledge everything, and lost what they were owed. Paths relative to
`src/agents/cassini/`. Each fix came with a test that failed first, and fails again with the fix mutated away.

**A. A processor was cut for acknowledgements the broker had not read (confirmed).** A client that publishes and
subscribes has its acknowledgements behind its own publishes on its one connection. The read-around stopped at the
first frame that was not an acknowledgement, so a processor publishing small frames back to back always had one
behind the waiting publish; behind a large frame waiting for its admission nothing at all is read; and on the client
a blocked large write holds the writer, so the acknowledgement writer waits too. The processor showed no progress, the
watchdog ended its connection, and the next alarm shed what it was owed.

*The choice:* of the three ways the review offered, the first, made safe. Progress is the connection's (B below), and a
connection whose reader waits for an admission is not ended for a stall: its missing acknowledgements are the broker's
doing. Spared alone, though, such a connection could hold an exhausted budget for good (two processors each waiting
behind the other's deliveries, or one behind its own), so a spared connection's reader *refuses for now*, for the
stall bound, each publish it cannot admit at once and the one it waits for (`PublishRefusal::Unavailable`, which a
facade keeps and sends again), and reads on, so its acknowledgements are read. A refused large frame's body is read
past under one global slot and let go at once (`REFUSED_BODY`, within the stall bound), so the budget is passed by at
most one frame, and only while connections refuse. The second way (a bounded queue of frames read ahead) cannot bound a
run of publishes before the acknowledgements and holds what it reads outside the budget; the third (a separate write
path on the client) cannot help while the broker does not read the connection, as everything the client writes is in
that one stream: the client's blocked write ends as soon as the broker reads on. Goal 2 (pressure over TCP) stays the
rule; refusing for now is the exception, for a connection the broker cannot otherwise make progress on.

*Where:* the reader marks itself while it waits (`ConnWriter::is_admitting`, `broker/src/listener.rs:136`;
`admit_or_refuse`, 391). `ConnWriter::spare` (153) spares a connection whose reader waits, and spares it again only
once it has made progress since, so a client that acknowledges nothing is ended at the next look. The watchdog spares
before it ends, deciding once per session a look (`budget.rs:448-470`); a topic's stall rule spares too, and gives the
subscriber the stall bound again to drain (`topic.rs:769`, `Backlog::restart_stall_clock`, `types/src/held.rs:307`).

*Tests* (`broker/tests/backpressure.rs`): `a_processor_publishing_small_frames_back_to_back_under_an_exhausted_budget_is_not_cut`
and `a_processor_publishing_large_frames_under_an_exhausted_budget_is_not_cut` (nine topics, each under its high water,
read at half a MiB a second by slow subscribers while the processor publishes to them and reads them: no one is cut,
the processor is given everything and every publish is accepted; with B fixed first both failed on the processor cut
and its deliveries shed; mutations: no sparing, the refusal ignored, the waiting reader unmarked, the decision per
backlog instead of per session); `a_processor_holding_back_its_own_topic_is_not_cut_for_acknowledgements_the_broker_has_not_read`
(the topic's rule, under a budget it cannot exhaust; mutations: no sparing, no fresh bound);
`a_client_that_publishes_and_never_acknowledges_is_spared_once_and_then_ended` (the bound on sparing; mutation: spared
without progress).

**B. Progress was one subscription's (plausible, confirmed by its test).** A client acknowledging a large backlog on one
topic was ended for a delivery on another waiting behind those writes past the stall bound; and the acknowledgements
themselves wait in the Listener's mailbox behind its writes, so the subscriber learns of them late. Progress is now the
connection's, recorded by its reader when it reads an acknowledgement (`ConnWriter::idle_for`, `listener.rs:177`), and
the watchdog spares a connection that made any within the bound. *Test:*
`a_client_acknowledging_one_topic_is_not_cut_for_a_delivery_waiting_behind_it_on_another` (fails with no connection
progress, or with it not recorded). The slow subscribers of A's tests were themselves cut before this fix.

**B, the poison delivery.** A frame the client cannot decode names nothing the client can read, so a delivery in it was
never acknowledged, held, and sent again on every resume. Both ends now number a connection's frames from one (the
Listener's writes, `listener.rs:1193`; the client's reader); the client names a frame that does not decode
(`ClientMessage::FrameUndecodable`, the last variant, so no tag moves; `TcpClientActor::on_frame`,
`client/src/lib.rs:877`), and the Listener lets the delivery it carried go as acknowledged, with a warning naming the
identity (`listener.rs:1681`). *Tests:* `a_delivery_the_client_could_not_decode_is_let_go_when_it_says_so` (fails with
the notice ignored, or with frames numbered from zero) and the client's `a_frame_that_does_not_decode_is_named_to_the_broker`.

**C. Deferred resends chained (confirmed).** A deferral armed a resend at the first backoff every time; a registration
before it fired sent the publish twice, and each deferral added a chain. The facade keeps one deferral per publish
(`Deferral`, `client/src/facade.rs:213`): its wait doubles from 1 s to 30 s with each deferral, a later one replaces the
armed timer (`Resend { timer }` does nothing unless it is the armed one), a registration cancels it (its retransmit
sends the publish), and an acknowledgement or refusal forgets it. *Tests:*
`a_deferred_publish_is_sent_again_once_however_deferrals_and_registrations_fall` and
`a_publish_deferred_again_waits_longer_each_time_up_to_the_last_backoff` (mutations: the timer not cancelled, a
replaced timer still firing, no growth).

**D. A bare transport said it would resend (confirmed).** `PublishDeferred`'s reason now says who sends it again: kept
and sent again through a facade (`DEFERRED_AND_KEPT`), or not kept, so its caller must (`DEFERRED_NOT_KEPT`,
`client/src/lib.rs:68`). *Test:* `a_deferral_says_who_sends_the_publish_again`.

**E. A wait dropped by a cut stayed counted.** `Budget::charge` counts a waiting reader with a guard (`Waiting`,
`budget.rs:163`). *Test:* `a_wait_given_up_is_no_longer_counted`.

**F. The watchdog ran through the drain.** The broker tells the budget when its drain begins (`Budget::begin_drain`,
`budget.rs:258`, from the first shutdown phase), and the watchdog stops. *Test:* `the_watchdog_stops_when_the_drain_begins`
(the budget's; the broker's one call is covered by reading only).

**Known limits added:** a client that never reads blocks its own Listener on a write, so its publishes wait in that
Listener's mailbox and never reach a topic: nothing is held back, and it holds up to its window and high water
uncut until the budget is exhausted, when the watchdog ends it (spared at most once). Publishes refused for now on a
bare transport are told to its caller and not sent again. The client's frame count in its read loop is not reached by a
test (the broker never writes a frame that does not decode); `on_frame`, which it numbers, is.

## Open decisions

1. **A subscriber that stays slow** past `T_stall`: end its connection (recommended; lossless until then, and the
   topic moves again), drop what it would be owed past its high water (lossy, never stalls anyone), or a choice per
   subscription (both, with more to configure).
2. **Sizing:** derive the budget from the container's memory limit (recommended: a share of it, with the per-connection
   window and per-subscriber high water fractions of the budget), or fixed settings.
3. **The frame limit:** the largest legitimate message decides it (harness runs use 1 MiB; git diffs and signed
   documents may be larger).
4. **Order:** phase 1 now, phase 2 with reliability step 2 (recommended), or both together.
