# Cassini client reliability

Status: **design, nothing built.** Written 2026-10-08 from a design discussion. It describes what the
Cassini client does today, why that loses messages, and the structure that replaces it. The open
decisions are in section 6.

## 1. What happens today

An agent publishes through `TcpClient` (`lib/src/cassini.rs`), a thin wrapper over the
`TcpClientActor` mailbox (`cassini/client/src/lib.rs`). One actor is both the API and the
connection.

- **`publish` is a cast to the actor's mailbox.** It succeeds whenever the mailbox is open, whether or
  not the broker is reachable. Only a closed mailbox (the actor is dead) reaches the offline policy.
- **The offline policy** (`OfflineBehavior`): `Fail` returns `BrokerUnavailable`; `Drop` returns Ok
  and discards; `Queue`, the default at almost every call site, appends a JSON line (`QueueEntry`:
  topic, base64 payload, timestamp, `attempts: 0`) to the file at `CASSINI_QUEUE_PATH`, or returns
  `BrokerUnavailable` if that is unset. No deployment sets it, so for agents `Queue` is dead.
- **Nothing replays the queue for agents.** `MessageQueue::drain()` has no caller on the agent path;
  only the CLI daemon (`cli.rs`) has its own append-and-replay.
- **While registered**, a write failure stops the actor (`emit_transport_err_and_stop`): the message
  that failed to write is lost, and so is everything in the mailbox.
- **While unregistered** (starting, or after a disconnect), `handle_unregistered_fallback` logs a
  warning and drops the message.
- **At the broker**, the topic actor queues messages in memory when a topic has no subscribers, and
  sessions are in memory, so a broker restart loses both.
- **The acknowledgement** is `PublishRequestAck { topic, trace_ctx }`, sent when the topic actor has
  accepted the publish (not when subscribers have received it). The client surfaces it as
  `ClientEvent::PublishAcknowledged { topic }` and correlates it with nothing, because it carries no
  message identity.

The system is effectively at-most-once, with silent loss in at least four places. The dead `Queue`
path and the missing replay are bugs in their own right.

## 2. The structure

Split what `TcpClientActor` does into a stable part and a disposable part.

![components](sequences/general/cassini_client_components.png)

- **Facade + Outbox** is what the rest of the agent holds, and it lives as long as the agent. It owns
  the agent-facing mailbox, the session identity (registration id), the remembered subscriptions and
  the outbox: a queue of messages not yet acknowledged by the broker, each with an `(epoch, seq)`. It
  supervises the transport, so respawning it is internal: the infrastructure supervisor's respawn
  logic, the `InfrastructureRespawned` handler in each domain supervisor, and the crypto actor's
  `ClientReplaced` message all go away. It begins as one actor with the outbox as a module, and splits
  only if it grows.
- **Transport** is the TLS connection with its reader and writer. It is disposable: any failure ends
  it and the facade respawns it, with backoff (which is also the respawn-storm guard).
- **Outbox file** holds the unacknowledged entries while things are not well.

## 3. Message flows

### Publishing, online

![publish](sequences/general/cassini_client_publish.png)

A message gets its `(epoch, seq)` when it enters the outbox and leaves it only when the broker
acknowledges that exact message. The ack is cumulative: acknowledging `n` releases everything up to
`n`. Writing to the socket proves nothing about the broker.

### Losing the connection, and coming back

![recovery](sequences/general/cassini_client_recovery.png)

At the first sign of trouble the facade writes the unacknowledged entries to the file and goes
Offline. While offline every publish goes into the outbox and is appended to the file. When a
connection is back it registers (resuming the session, or fresh if the broker lost it), re-sends the
remembered subscriptions, retransmits the outbox in order with the same epoch and seq, and removes
the file once the outbox is empty.

### Deliveries from the broker

![inbound](sequences/general/cassini_client_inbound.png)

The same problem in the other direction. The broker keeps a delivery until the client acknowledges it
(`PublishResponseAck`, which exists in the protocol), and the facade drops a repeat by its origin.

### The facade's states

![states](sequences/general/cassini_client_facade_states.png)

`Draining` is the agent's shutdown or rejuvenation: the facade keeps retransmitting until the outbox is
empty or the shutdown budget is spent, then writes any remainder to the file. The budget therefore has
to include this time.

## 4. Duplicates

A retransmit after a lost acknowledgement delivers a message twice, so delivery is at-least-once
unless something removes the duplicates. The technique that needs no stored set of ids is a per-sender
sequence inside an epoch (the idea behind Kafka's idempotent producer and MQTT's QoS 2):

- The facade picks a random **epoch** each time it starts and numbers messages 1, 2, 3 within it.
- A receiver keeps one integer per sender and epoch, the highest sequence it has accepted, and drops
  anything at or below it. That is constant state, discarded when the sender's session ends.
- Retransmission sends the unacknowledged in order, before anything newer, so the order a receiver
  sees is increasing.
- Entries carry the epoch they were numbered in, also across a restart: an entry loaded from the
  file keeps its original epoch and seq, and only new messages use the new epoch.

If the origin `(publisher identity, epoch, seq)` travels with the message to the subscriber, the same
rule removes duplicates at every hop, including after a broker restart that lost its own record. No
hash and no clock is needed for deduplication. Replay protection against an attacker is a different
matter, handled by mTLS and, for configuration, by signed monotonic versions.

## 5. Protocol and broker changes

- `PublishRequest` gains `epoch` and `seq`; `PublishRequestAck` returns them (cumulative).
- Deliveries (`PublishResponse`) carry the origin `(published_by, epoch, seq)` and a delivery id;
  `PublishResponseAck` returns the delivery id.
- The broker keeps, per client session, the highest accepted `(epoch, seq)`, and a per-subscriber
  outbox of unacknowledged deliveries.
- The per-topic queue the broker holds when there are no subscribers, and sessions, are in memory
  today. Whether they persist is a separate decision (section 6).

## 6. Open decisions

1. **Where duplicates are removed:** at every hop by the origin tuple (section 4), or only
   client-to-broker first.
2. **Where the outbox file lives.** An `emptyDir` survives a container restart but not the pod
   replacement that every rejuvenation is, so unsent messages left at pod end would be lost. Either
   the drain flushes the outbox within the shutdown budget (and leftovers are accepted as lost when
   the pod is killed hard), or the file lives on a volume that outlives the pod, as the credentials
   agent's ledger already does.
3. **Broker persistence** of sessions and topic queues. Without it a broker restart still loses
   whatever it held.
4. **Bounds.** The outbox needs a size limit and a rule when it is full. `OfflineBehavior` becomes the
   caller's statement of that rule: `Queue` (wait for the connection), `Fail` (error now), `Drop`
   (best-effort).
5. **Receipts.** Whether a publisher can ask to be told its message was acknowledged.

## 7. Build order

1. Facade and transport split, no behaviour change: the facade owns the mailbox and the
   transport's respawn, and the domain supervisors drop `InfrastructureRespawned`.
2. The outbox with an in-memory queue, and the acknowledgement path, using a message id added to the
   existing ack.
3. The offline file: quiesce on trouble, rehydrate on start, replay.
4. Dedupe at the broker and at subscribers, with the epoch and sequence.
5. The broker's per-subscriber outbox and, if decided, persistence.
