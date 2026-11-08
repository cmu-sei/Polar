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
- **The stall bound** (*revised 2026-11-01*: only a subscriber that acknowledges nothing for `T_stall` is ended; see
  "The stall rule ends only a subscriber that makes no progress"): a subscriber above high water for longer than
  `T_stall` is slow. The rule is decision 1 below;
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
   *Revised 2026-11-01:* only one that acknowledges nothing for `T_stall`; a slow one that keeps acknowledging is held
   back by its topic, never ended ("The stall rule ends only a subscriber that makes no progress").
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
  `cassini/broker/tests/backpressure.rs` tests. (Since 2026-10-31 a plan's `sink_slow: "before_ack"` spends the delay
  in the sink's client's reader before each acknowledgement, `cassini_client::hand_off`; broker-baseline.md, the fifth
  set.)

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

*The two processor tests' exhaustion, made certain (2026-10-31).* The large-frames test failed about one run in three
alone on main, and every run under load, at its first check, "the budget was exhausted": the rest of the test never
ran. Its 270 frames of 262,320 bytes are 70.8 MB against a budget of 67,108,864, so the budget is exhausted only if the
slow subscribers have had at most 14 messages acknowledged when the processor's last publish is read. Measured with the
budget's counters every 100 ms: alone, a passing run's alarm rose at 269 charges and exactly 14 refunds (1.3 s of
publishing); under load (three copies and eight busy loops) the processor took 4.5 s to publish, 46 were refunded
meanwhile, and the budget peaked at 58.8 MB, with no reader waiting and nothing refused: the broker took every
publish as it came, so the processor was not starved, and nothing was cut. The test assumed the processor outruns the
slow subscribers by 14 messages; the broker was not at fault. Now the processor publishes its rounds back to back as
before and then, until the budget is exhausted, one more round at a time, each once the broker has taken every
publish before it and only while every topic stays under its high water (what the test is about is unchanged: each
topic under its high water, every subscriber making progress, the budget exhausted); the checks after are the same,
over the rounds it published. Before, 0 of 9 under that load; after, 9 of 9, and the counts in `remaining-work.md`.

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

## Decisions reviewed with the maintainer (2026-10-30)

The maintainer reviewed the decisions of the three rounds with the coordinator. Paths relative to `src/agents/`.

**Kept as built:** the budget's defaults (13; since replaced by the runtime budget below, its floor four frames); the
dead-letter queue replaced by the delivery outbox (15); an untyped refusal treated as `Denied` (16); the sparing of a
connection whose reader waits, the one-frame overshoot of a refused large frame, and the bound on a client that never
reads (18, 22, 24); `FrameUndecodable` (23, sound once the protocol version below exists).

**Changed:**

1. **The watchdog is one supervised actor, and the stall policy is one function.** Three rounds of special cases on
   one loop said the shape was wrong: a bare task outside the supervision tree, finding the session manager through the
   registry and the Listener through a mutex in the budget, setting subscriber state from outside, and duplicating the
   topic's stall rule. Now:
   - **The `PressureManager`** (`cassini/broker/src/pressure.rs:79`) is spawned linked under the broker
     (`broker.rs:691`). It ends with the drain (`PressureMsg::Drain`). If it ends or fails otherwise, the broker starts
     a new one (`broker.rs:717`), but never while the broker itself stops.
   - **It writes no other actor's state.** At each stall check (`look`, `pressure.rs:213`) it tells the subscriber
     manager, which tells each subscriber: `BudgetExhausted` while the budget is exhausted, `StallCheck` otherwise.
   - **A subscriber looks at its own backlog** (`look_for_stall`, `subscriber.rs:897`). It reports a stall when it is
     above its high water for the stall bound, or, under an exhausted budget, has had nothing acknowledged for it. It
     tells its session (`SubscriberStalled`). Away under an exhausted budget, it sheds (`shed_if_away`,
     `subscriber.rs:929`).
   - **It tells its topic whether it holds the topic back** (`TopicHold`, `subscriber.rs:880`, to the ref the topic
     gave it in `TopicAttached`). The topic reads no subscriber's atomics and has no stall check of its own
     (`topic.rs:697`).
   - **The session decides by the one policy**, `pressure::verdict` (`pressure.rs:279`), from its own facts
     (`judge_stall`, `session.rs:921`).
     - Its connection's reader tells it, beside the Listener's mailbox, when it reads an acknowledgement and when it
       waits for room (`ConnectionProgress`, `ConnectionWaiting`; `listener.rs:111, 157`).
     - Its Listener gives it the connection's control (`ConnectionOpened`, a weak handle, `listener.rs:188`), so the
       connection still closes with its Listener.
     - It spares (refuse for now) or cuts the connection. A connection it ends is logged with its identity by the
       session manager, and counted by the subscriber.
   - **The budget is accounting only.** Its list of holders is read for its report alone.
   - **Tests.** The finding tests were the specification and pass with their assertions untouched (plumbing changes are
     listed in the commit). New: `the_pressure_manager_is_started_again_when_it_ends_and_the_checks_go_on` (fails
     without the restart), the policy's example test, and two properties (`a_connection_is_ended_only_when_nothing_spares_it`,
     `a_client_that_never_progresses_is_spared_at_most_once`).
   - **Mutations killed:** no refusal window, no connection progress, no sparing, progress or waiting not kept, no
     exhausted rule, no high-water rule, the topic never told.
   - **Removed:** one mutant survived, restarting a spared subscriber's stall clock (the refusal window covers it). It
     was removed.
   - **Also stopped:** a stalled subscriber is now judged when it is above its high water for the stall bound, whether
     or not a publish waits on its topic (the topic's rule ran only while one did).
   - **A two-step end.** The reader's facts reach the session by message, in order with the stall reports. A reader
     that began waiting just after its subscriber reported was judged on facts a moment stale, and its processor was
     ended rather than spared. This was found when the suite ran without logging, failing two runs in three; round 3's
     tip passed three in three. Now a connection the policy would end is first condemned (`Verdict::Condemn`). It is
     ended only by a later look, at least an eighth of the stall bound on (`pressure::confirmation`), that still finds
     so; a look that finds otherwise clears it. This gives the reader's facts a check's time to arrive, and delays a
     real end by at most one check. After the change, six runs in six passed.
2. **(14) A session shed under an exhausted budget ends, and its client is told.**
   - **The flow.** A subscriber that sheds tells its session (`SessionShed`). The session asks to be ended
     (`EndSession`, `session.rs:271`). The session manager logs it with the identity and keeps the reason for the
     retention window.
   - **The client is told.** A registration asking to resume that session is answered first with
     `ClientMessage::SessionEnded { registration_id, reason }` (`session.rs:747`; the last variant, so no tag moves),
     then given a new session. The client raises `ClientEvent::SessionEnded`.
   - **jira-observer** resynchronises: its next issue pull starts from the graph's cursor, asking the state agent again
     (`JiraObserverMessage::Resync`, `jira/observe/src/issues.rs`).
   - **The other agents**, on a new session today: the agent builder resubscribes every `with_subscription` topic and
     asks for its configuration again (`ConfigurationReplayRequest`). What a subscriber was owed is lost, and none of
     them replays it. By agent:
     - jira-processor, kube-consumer, git-processor, provenance resolver, polar-scheduler processor, git controller and
       soft-state: health only (the builder resubscribes); held messages are lost.
     - kube-observer and git-repo-observer (whose repository set is re-announced every 60 s) heal by their own polling.
     - gitlab-observer, gitlab-consumer, openapi-observer, openapi-processor and the build processor (provenance
       linker): spawn named actors on every `Registered`, so a second registration collides on names. That is a
       pre-existing fault, recorded here, not fixed. *Since 2026-10-31* gitlab-consumer is safe: its consumers
       subscribe in `pre_start`, so keeping them would leave them deaf on the new session; at a registration after the
       first the supervisor stops them and its graph controller, waits for each, and starts them again, and the stops
       (reason `re-registered`) are not taken for failures (`ConsumerSupervisor::stop_children`,
       `gitlab/consume/src/supervisor.rs`; tested by registering twice).
     - polar-scheduler observer: replays what it queued; a replay request missed while away is lost.
     - polar-custody daemon: re-pulls its authority.
   - **Test:** `a_subscriber_away_when_the_budget_is_exhausted_lets_go_of_all_it_was_owed` now asserts the returning
     client is told, with why, before a new session (it asserted a resume before; the decision changes that).
3. **(17) git-repo-observer publishes oldest first, in bounded batches.**
   - **The walk.** A ref is walked in reverse topological order (every commit after its parents), at most `max_depth`
     from the tip, holding only commit ids (`plan_ref`, `git/observe/src/lib.rs:566`). It is published
     `POLAR_GIT_PUBLISH_BATCH` (32) events at a time.
   - **The checkpoint** (`Checkpoint`, `lib.rs:546`) moves after each batch taken.
     - It is the frontier of what was published (the published commits no published commit descends from), not the
       batch's last commit alone. With merges, the last commit hides only its ancestors, and a sibling branch's commits
       published before it would be published again.
     - On a linear history the frontier is the last commit; when the whole ref is published, its tip.
     - The walk hides it, so after a partial walk nothing is lost and nothing taken is walked again.
   - **Tests:** a history with two merges cut after its first batch, and a property over random DAGs, batch sizes and
     cut points.
   - **Mutations killed:** the last-commit checkpoint (caught by the property), newest first, the checkpoint not moved,
     not hidden.
4. **(19) A protocol version at registration.**
   - **The version.** `cassini_types::PROTOCOL_VERSION` (1) is sent in `RegistrationRequest { protocol }`.
   - **A mismatch is refused by name.** `RefusalCode::ProtocolMismatch` gives both versions, the broker logs it with the
     identity and closes the connection (`listener.rs:1554`). A refused client waits `PROTOCOL_RETRY` (30 s) before
     each reconnect.
   - **An older client:** its registration (laid out without the field) does not decode. The broker answers its first
     undecodable frame with a refusal of code `Other`, which it can decode, naming the protocol, and closes. It logs
     "Failed to register session with the server. protocol mismatch: …" and emits `RegistrationRefused`. A bare
     transport then stays idle; a facade retries with its backoff.
5. **(20)** `polar-scudo/tests/every_clippy_toml.rs` fails if any clippy.toml in the workspace lacks
   `await-holding-invalid-types` for `tracing::span::Entered` and `EnteredSpan`.

## Runtime budget (2026-10-30)

**Decided** (the maintainer): memory is a shared resource the broker may not control. The budget is set at run time
from what the host and the container show and from how fast subscribers drain, not fixed at start. The pressure manager
is its controller (`cassini/broker/src/runtime_budget.rs`, pure, given the time, memory's sample and the budget's
counters; `PressureManager::sample`, `pressure.rs:169`).

- **Floor:** four frames, 256 MiB (`Tunables::floor_frames`).
  - **At start**, the lesser of `memory.max − memory.current` (when there is a limit) and `MemAvailable` must give it,
    or the broker does not start, naming the numbers and what to give (`startup_check`, `runtime_budget.rs:201`). A cap
    under it is refused too.
  - **At run time**, a ceiling below it starves the budget: it admits nothing (`Budget::set_starved`), says so, and
    recovers when memory returns. The broker does not exit.
- **Ceiling,** sampled every second through a `MemorySource`: the least of
  - a share of `MemAvailable` above a reserve, plus what is held: `held + (MemAvailable − reserve) × share`;
  - under a container limit, half of `memory.max` (`limit_share`), and never more than `memory.max − (memory.current −
    held)`, the room the broker's working memory leaves;
  - under pressure, the budget cut by a factor. Pressure is PSI `some avg10` above a threshold, or new `high` or
    `oom_kill` counts in `memory.events`. A first reading of the counts is not news. Both files are optional.
- **Target,** Little's law: the smoothed drain rate (bytes refunded a second) × the latency target. The latency target
  is the stall bound: the stall rule acts at the bound, so holding more than the subscribers drain within it buys
  backlog, not throughput.
- **The budget** starts at the floor, there being no demand yet. Each sample it:
  - grows by a step while it is used (held at its used share) and below the target;
  - falls by a step toward the target when it is over the target by more than the hysteresis;
  - is cut by the factor on pressure;
  - is clipped to the floor, the ceiling and the cap. `CASSINI_MEMORY_BUDGET_BYTES` is now that cap.
- **A shrinking budget evicts nothing:** held over the new limit, the alarm stops admission until it drains
  (`Budget::set_limit`, `budget.rs:267`). Windows and high waters follow the budget as it is now, keeping their ratios
  and minimums (`Bounds::window`, `high_water`, `budget.rs:95, 100`; `Window::of`; a subscriber's
  `Backlog::set_high_water` at each check).
- **A broker built from `Bounds` directly** (a test's) keeps a fixed budget.

**Defaults, MARKED FOR THE MAINTAINER'S REVIEW** (`Tunables`, `runtime_budget.rs:132`). Reviewed 2026-10-31: they
stay as set until the benchmark (saturated and slow-sink loads) informs them; every one is a variable. Measured
2026-10-31 (broker-baseline.md, the fifth set, with its recommendations): the latency target and the step are settings
now (`CASSINI_BUDGET_LATENCY_PERCENT`, `CASSINI_BUDGET_STEP_BYTES`, defaults unchanged); no load the harness can make
grew the budget off its floor.

| Tunable | Default |
|---|---|
| sampling interval | 1 s |
| floor | 4 frames (256 MiB) |
| share of `MemAvailable` above the reserve | 1/2 |
| reserve the host is never pushed below | 512 MiB |
| PSI `some avg10` threshold | 10 % |
| growth (and fall) step | 16 MiB a sample |
| cut factor on pressure | 1/2 |
| latency target | 1 × the stall bound |
| smoothing of the drain rate | 1/4 of each new measure |
| used share (above which it may grow) | 3/4 |
| hysteresis over the target | 5/4 |

**What u7c exposes** (the broker's cgroup, read 2026-10-30): `memory.max` = max, `memory.high` = max, `memory.current`
≈ 27 MiB. There is no `memory.pressure` (no PSI in that nested, rootless cgroup). `/proc/meminfo` shows the host:
`MemTotal` 62 GiB, `MemAvailable` 49 GiB. There the ceiling is the `MemAvailable` share, and pressure comes only from
`memory.events`, when the file is present.

**The broker's memory limit, 1 GiB** (the maintainer, 2026-11-01; broker-baseline.md, the fifth set: without one the
ceiling was 27.5 GB on u7c). The catalog gives the broker's container a request and a limit of 1 GiB each
(`infra/config/workloads/cassini.nuon`, `memory: { request: "1Gi", limit: "1Gi" }`). The request is the limit: no
convention in the schema said otherwise (no workload set any resources before), and what the scheduler sets aside is then
what the broker may use. The render takes it as any workload's `memory` (`infra/schema/functions.dhall`, `memoryOf`, in
both `agent.dhall` and `ca.dhall`; `resolve.nu` refuses a quantity it cannot read or a request over its limit,
`rules.nu`, `memory_problems`), and a target may give a workload other memory for a machine that has other. The
container's limit is its cgroup's `memory.max`, which `SystemMemory` reads (`runtime_budget.rs:88`); then:
- **The floor holds at start:** `memory.max − memory.current`, 1 GiB less the broker's memory at start (tens of MiB),
  is well over four frames (256 MiB); it would not be once the working memory passed 768 MiB.
- **The ceiling is half the limit** (decided by the maintainer 2026-11-01): 512 MiB of a 1 GiB limit, the other half
  left for the broker's own working memory, which grows with load (the fifth set's broker sat at 17 to 22 MB between
  runs and peaked at 56 to 115 MB); never more than `memory.max − (memory.current − held)`. First built as the limit less
  the working memory (about 994 MiB at rest), which left a load spike in working memory to meet an OOM kill between two
  samples. A limit whose half is under the floor (four frames) is refused at start, saying to give at least twice the
  floor. Tested: `runtime_budget.rs`'s `under_the_catalog_s_one_gib_limit_the_broker_starts_and_its_ceiling_is_half_the_limit`
  and `a_broker_below_its_floor_of_four_frames_does_not_start_and_says_why`.
- **For the maintainer:** that ceiling leaves the broker only what is outside the budget at the last sample (one a
  second) between its budget and the kernel's OOM kill, while a write's copy of a frame, TLS buffers and the uncharged
  per-message overhead (review finding 6) grow with load. If the ceiling under a limit should be a share of it (half, as
  the share of `MemAvailable` is), that is a change to the controller, not made here.
- **Not verified on a cluster:** that u7c's nested rootless cgroup enforces and shows the limit (`memory.max` read
  `max` there before any limit was set).

**Visible:**
- The bounds' report (use, peak, readers waiting, refusals, slow-subscriber disconnections, alarms, the largest
  holders) is the pressure manager's own, every 30 s (`PressureMsg::Report`; `budget_report::report`), and what it
  last reported is answered (`PressureMsg::Reported`): no task outside the supervision tree reports or decides.
- The controller logs its decision (budget, ceiling, target, what limited it, drain rate, held, alarm, starvation) when
  what limits it changes, and every ten samples otherwise. It warns when memory falls below the floor.
- `PressureMsg::Snapshot` answers a typed `BudgetSnapshot` (budget, ceiling, target, `LimitedBy`: `Floor`,
  `Ceiling(MemAvailable | Limit | Pressure)`, `Target`, `Cap`; drain rate, recent pressure events, starvation, alarm,
  held) for pulled health to read.
- `PressureMsg::WarmStart` takes a learned value (the last stable budget and drain rate) whenever it comes, within the
  floor, ceiling and cap. Nothing waits for it. A restarted manager warm-starts from the budget as it is.

**Tests:**
- `runtime_budget.rs`'s: start-up refusal with the numbers; missing PSI and events; the target follows the drain rate
  and quiet use grows the budget; pressure cuts; starvation and recovery; warm starts clipped, late or never; a property
  that the budget lies within its floor, ceiling and cap.
- `budget.rs`'s: a shrinking budget evicts nothing and admits again once drained; starvation; a window follows its
  budget.
- `pressure.rs`'s: the manager sets the budget, takes a warm start, tells its state, and leaves a fixed budget alone.
- Mutations killed: no ceiling clip, no cut, no floor, the target not from the stall bound, never starving, no cap,
  shrinking or starving still admitting.

**Planned next, not built:** the broker publishing in-process into its own topics, stamped with its own identity, so
that soft-state's StateActor and pulled health reach it. The learned warm start is to come from soft-state, which itself
needs the broker; hence nothing waits for it.

## The stall rule ends only a subscriber that makes no progress (2026-11-01)

**Decided** (the maintainer, 2026-11-01; broker-baseline.md, the fifth set, (c)): a subscriber is ended only when it
acknowledges nothing for `T_stall`. A slow subscriber that keeps acknowledging is never ended: its topic holds back
through its high water, as before, so its publishers go at its pace. Until then the rule ended a subscriber above its
high water whose outbox could not fall to half of it within `T_stall`, acknowledging or not: a drain-rate threshold of
(high water / 2) / `T_stall`, 0.56 MB/s at the floor and the default bound, which rose with the budget.

**The change** is to the one policy and its facts (paths relative to `src/agents/cassini/`):
- `pressure::verdict` (`broker/src/pressure.rs:311`): a connection that made progress within the bound (an
  acknowledgement read on any topic) is never ended, spared nor condemned, whether its subscriber was seen above high
  water or under an exhausted budget. Before, progress excused it only under an exhausted budget.
- `StallFacts` (`pressure.rs:265`) loses `exhausted`, which only that distinction read, and so does the message that
  carries a subscriber's report (`BrokerMessage::SubscriberStalled`, `types/src/lib.rs`) and `judge_stall`
  (`broker/src/session.rs:941`).
- **Kept:** the refusal window, the sparing of a connection whose reader waits for room (again only after progress), the
  two-step end (condemned, then ended at a later look at least an eighth of the bound on), progress being the
  connection's, and what a subscriber reports (`look_for_stall`, `subscriber.rs:914`: above its high water for the
  bound, or holding anything with nothing acknowledged for the bound while the budget is exhausted). The report is now a
  "may have stalled", which the session ends only on its connection's lack of progress.

**Can acknowledging but very slow subscribers hold an exhausted budget for good?** No.
- While the budget is exhausted no charge is taken (`Budget::try_charge` refuses under the alarm), so what it holds
  never grows, and every acknowledgement that lets a message's last holder go refunds it. What waits in a topic was
  charged when its reader admitted it; a topic resuming moves it into outboxes and charges nothing.
- Progress under an exhausted budget is finite unless it refunds: each acknowledgement removes one delivery the broker
  holds, and with nothing admitted there are only so many. A connection that runs out of deliveries to acknowledge while
  it still holds anything makes no progress, and is ended at the bound; away, it lets go of all it held at the next
  look (`shed_if_away`). So the truly stuck are still ended by the "no progress" path, and a session away still sheds.
- The topic hold bounds what any one subscriber can hold to its high water and what waits for it in its topic (its
  publishers' windows, all charged to the budget); admission refusal stops everything else.
- **So it always recovers, but at the slow subscribers' pace, not within `T_stall`:** the time to fall to the low water
  is what must be refunded over the rate they acknowledge, plus the bound for any that acknowledge nothing. Under the
  old rule a slow subscriber above its high water was ended after the bound; now, as subscribers under their high water
  always were, it is waited for. Meanwhile the alarm stops every reader, on every topic. A pathological set (subscribers
  each acknowledging one small message just within every bound) could keep the budget exhausted for hours: bounded, not
  for good, and a cost the decision accepts.

**Tests** (`broker/tests/backpressure.rs`), each failing first against the old rule:
- `a_slow_subscriber_that_keeps_acknowledging_is_not_ended_and_holds_its_topic_back` (221): half a MiB a second, under
  0.56 MB/s, above its high water for four stall bounds; not ended, its topic held back and its publisher held to its
  pace, then given everything in order. Before: ended in its slow phase.
- `a_subscriber_that_stops_acknowledging_is_ended_the_stall_bound_after_its_last_acknowledgement` (307): slow for three
  bounds, then acknowledging nothing: ended, no sooner than the bound after its last acknowledgement; the publisher
  completes. Before: ended while it still acknowledged.
- `slow_subscribers_that_keep_acknowledging_cannot_hold_an_exhausted_budget_for_good` (362): seven slow subscribers, each
  above its high water at a quarter of a MiB a second, and one that reads nothing, exhaust a 64 MiB budget; it stays
  exhausted longer than the bound, what it holds never grows meanwhile, the silent one alone is ended, the budget
  recovers, and the slow ones are given everything in order. Before: all eight were ended.
- **Changed because the rule changed** (`broker/src/pressure.rs`): the example test's assertion "above high water,
  progress does not excuse it" expected `End` for a connection that made progress (seen above high water); it now
  expects `Ignore`. The property `a_connection_is_ended_only_when_nothing_spares_it` counted progress only under an
  exhausted budget (`exhausted && since_progress < STALL`); it now counts it always, and also asserts that a connection
  that made progress is only ever ignored. Added to the example: a progressing connection whose reader waits is not
  spared. No other assertion was changed or removed.
- **Mutations killed:** the progress check removed (the example, the property, the three tests above and
  `a_client_acknowledging_one_topic_is_not_cut_for_a_delivery_waiting_behind_it_on_another` fail); the sparing checked
  before progress (the example and the property fail).

## Open decisions

1. **A subscriber that stays slow** past `T_stall`: end its connection (recommended; lossless until then, and the
   topic moves again), drop what it would be owed past its high water (lossy, never stalls anyone), or a choice per
   subscription (both, with more to configure).
2. **Sizing:** derive the budget from the container's memory limit (recommended: a share of it, with the per-connection
   window and per-subscriber high water fractions of the budget), or fixed settings.
3. **The frame limit:** the largest legitimate message decides it (harness runs use 1 MiB; git diffs and signed
   documents may be larger).
4. **Order:** phase 1 now, phase 2 with reliability step 2 (recommended), or both together.
