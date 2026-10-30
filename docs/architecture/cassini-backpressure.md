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

Status: **proposed 2026-10-30, not cleared.** What the broker and client bound today is in cassini-broker.md ("Bounds
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

## Open decisions

1. **A subscriber that stays slow** past `T_stall`: end its connection (recommended; lossless until then, and the
   topic moves again), drop what it would be owed past its high water (lossy, never stalls anyone), or a choice per
   subscription (both, with more to configure).
2. **Sizing:** derive the budget from the container's memory limit (recommended: a share of it, with the per-connection
   window and per-subscriber high water fractions of the budget), or fixed settings.
3. **The frame limit:** the largest legitimate message decides it (harness runs use 1 MiB; git diffs and signed
   documents may be larger).
4. **Order:** phase 1 now, phase 2 with reliability step 2 (recommended), or both together.
