# Agent Health and Shutdown

Status: **design agreed 2026-10-06; implementation pending.** Sections marked
*Today* describe the code on `feat/ace-integration`. Sections marked *Target*
describe the agreed design. The remaining-work item "Health system rework"
tracks the move from one to the other.

Scope: agents built on `polar-agent-builder`. Agents that have not migrated keep
their hand-written health wiring; they only need to keep compiling.

## 1. Principle

An agent's health has two layers, and they do not overlap.

| Layer | Owner | Job |
|---|---|---|
| **HealthCheckActor** | the agent, in-process | The authoritative judge of health. Covers every situation we care about: Cassini connection, graph connection, certificate lifetime, dependency certificate lifetime, rejuvenation. |
| **Kubernetes liveness probe** (`polar-healthcheck`) | kubelet | A dead-man's switch. It answers only "has the HealthCheckActor stopped reporting?". It exists for the one case where nothing in-process can run: a wedged or deadlocked process. |

In every other circumstance the HealthCheckActor decides, and the process ends
itself through an orderly path. A kubelet kill is the failure path of the health
system, not its normal mechanism.

A second principle follows from the first: **health decides *when* to stop; the
InfrastructureSupervisor decides *how* to stop.** SIGTERM is not a health
event, so it is not the HealthCheckActor's concern.

No agent ever exits with code 0. Every ending is non-zero so the Job
(`restartPolicy: Never`) replaces the pod, and `polar-init` issues fresh certificates.

## 2. Components

```
domain supervisor (per agent)
└── InfrastructureSupervisor            owns the shutdown pipeline
    ├── HealthCheckActor                judges health, writes the liveness file
    ├── RouterActor
    ├── TcpClient (Cassini client)
    └── CryptoActor                     only with_encryption()
```

`polar-agent-builder` wires this. The domain supervisor receives
`InfrastructureEvent::{Respawned, PrepareShutdown}` and replies with
`InfrastructureSupervisorMessage::ProceedWithShutdown`.

## 3. The HealthCheckActor

### Inputs

- Cassini connection state (`CassiniConnected` / disconnected).
- Graph connection state, only when the agent `expects_graph` (derived from
  `with_graph_write()`).
- Certificate lifetime of the agent's own client certs (`POLAR_HEALTH_CERTS`).
- Certificate lifetime of dependencies (`POLAR_HEALTH_DEP_ENDPOINTS`, derived
  from the catalog `needs`).

### Phases

```
Normal ──(cert TTL < rejuvenation threshold)──▶ WaitingForDeps ──(deps healthy)──▶ Jittering{fire_at} ──(fire_at)──▶ stop
```

- **Normal**: ticks every `POLAR_HEALTH_TICK_SECS`, evaluates health, writes the file.
- **WaitingForDeps**: the agent's certs are due for rejuvenation, but a
  dependency is not yet healthy, so rejuvenating now would restart into a
  failure. Wait.
- **Jittering**: a random delay inside the jitter window, so a fleet whose certs
  were issued together does not restart together.
- On `fire_at` the actor stops cleanly with a stated reason.

There is one tick chain. It starts when the actor starts, not when Cassini first
connects, so the file and the hard deadlines are live from the first tick. Each
tick schedules the next only when it has finished, and scheduling never blocks
the actor, so a message such as a Cassini disconnect is handled at once rather
than queueing behind a timer.

### Constants and environment

| Value | Default | Env override |
|---|---|---|
| Tick | 30 s | `POLAR_HEALTH_TICK_SECS` |
| Certificate expiry (hard floor) | 60 s | `POLAR_HEALTH_EXPIRY_SECS` |
| Rejuvenation threshold | 300 s | `POLAR_HEALTH_REJUVENATION_SECS` |
| Jitter window | 180 s | |
| Graph deadline | 120 s | `POLAR_HEALTH_GRAPH_DEADLINE_SECS` |
| Cassini disconnect deadline | 60 s | `POLAR_HEALTH_CASSINI_DEADLINE_SECS` |
| Probe stale threshold | 90 s | `POLAR_HEALTH_STALE_SECS` |

The ordering `rejuvenation > expiry` is an invariant. *Target:* a startup
consistency check validates the whole set (tick < stale threshold, jitter window
fits between rejuvenation and expiry, and so on) and the agent fails fast on a
bad combination rather than misbehaving later.

### The liveness file

The actor writes a JSON file each tick: `status`, `timestamp`,
`cassini_connected`, `graph_connected`, `phase`. The `phase` field (`normal`,
`waiting_for_deps`, `jittering`, `shutting_down`) is informational: it lets an operator reading
the file tell "rejuvenating" from "unhealthy", the probe ignores it, and files
written before it existed read as `normal`.

The file is written every tick in every phase, healthy or unhealthy according to
the connection state. (It used to be written only in the Normal phase, so a
rejuvenation that waited longer than the probe's 90 s limit let the file go stale
and the kubelet killed the pod before the graceful path ran.)

A write that fails is logged with its reason. The file is the probe's only
evidence, so a file that cannot be written would go stale and get the pod killed
without explanation; after three failed writes in a row the actor reports
itself unhealthy, with the reason, instead.

### Deadlines

Hard deadlines belong to the actor, not to the probe:

- **Certificate**: the agent's own cert is below the expiry floor.
- **Graph**: `expects_graph` and no graph connection within the graph deadline.
- **Cassini disconnect**: Cassini has been unreachable for longer than
  `POLAR_HEALTH_CASSINI_DEADLINE_SECS` (default 60 s), timed from startup until the
  first connect and from each disconnect until the next connect. A repeated
  disconnect does not restart the clock, and a reconnect resets it. The 60 s is
  about what the liveness probe amounted to before the actor owned this decision.
  It is tuned together with Cassini session repair (see section 7), because a
  longer grace period is only worth having once a reconnect really restores the
  agent's subscriptions.

A deadline, or a rejuvenation, is reported as an explicit verdict message,
`HealthVerdict::Rejuvenate` or `HealthVerdict::Unhealthy(reason)`, sent once to
the recipient the actor was given. The actor does not die to deliver it. It stays
alive and goes on ticking, and every tick refreshes the file with
`phase = shutting_down`, so the liveness probe does not kill the agent in the
middle of its drain. After the verdict it judges nothing further. A rejuvenating
agent still reports healthy while it drains; an unhealthy one reports unhealthy.

The same verdict is raised when the actor cannot write its file three times in a
row. An actor given no recipient (agents that have not moved to the builder, and
the Cassini broker) ends itself as before: it fails on a hard deadline and stops
for rejuvenation. The builder's infrastructure supervisor does not use the
verdict yet; that is the next stage of the rework.

## 4. The InfrastructureSupervisor shutdown pipeline

*Target.* One funnel, `begin_shutdown(reason)`, with every trigger feeding it.
The reason is logged each time.

| Trigger | Today | Target |
|---|---|---|
| Health verdict: rejuvenation reached | orderly | orderly |
| Health verdict: hard deadline (cert, graph, Cassini) | abrupt (actor fails) | orderly |
| HealthCheckActor dies unexpectedly (panic, bug) | abrupt | orderly, logged as a severe fault |
| Router / Cassini client / CryptoActor dies unexpectedly | abrupt | orderly |
| SIGTERM or SIGINT | ignored by PID 1 | orderly |
| `kubectl delete`, rollout, node drain | SIGKILL after the grace period | orderly, via SIGTERM |

Design decisions:

- **Verdict messages, not death.** The HealthCheckActor reports its decision with
  an explicit message and never uses its own termination as the signal. Its
  *unexpected* death is still treated as "something is very wrong" and triggers
  shutdown, but it is the exception path, not the protocol.
- **One pipeline.** `PrepareShutdown` → domain supervisor drains and stops its own
  children → `ProceedWithShutdown` → infrastructure children stop → process exits
  non-zero. Every trigger takes the same steps.
- **Shutdown is idempotent.** A second trigger while shutting down (for example
  SIGTERM during a rejuvenation drain) is logged and ignored.
- **In-process watchdog.** The supervisor waits a bounded time for
  `ProceedWithShutdown`. If the domain supervisor does not answer in time, the
  process exits 1 anyway. Previously the supervisor waited indefinitely and only
  the Kubernetes probe could end a hung shutdown. With the watchdog the probe
  covers only what it should: a process in which nothing can run.
- **Bounded steps.** Each step has its own timeout. Safety comes from the bounds,
  never from assuming the children are still alive.

### Termination grace

SIGTERM handling only helps if the kubelet waits long enough. The Kubernetes
default grace period is 30 s. `terminationGracePeriodSeconds` must exceed the
slowest drain. The credentials-agent consumer's worst case is about 45 s, so it
needs about 60 s. The catalog carries the value, and the two hand-written
credentials charts carry it until they move to the catalog.

## 5. The Kubernetes liveness probe

`polar-healthcheck` is the container's liveness exec probe. Chart settings:
`initialDelaySeconds` 150, `periodSeconds` 30, `failureThreshold` 2,
`timeoutSeconds` 5.

*Today* it exits 1 on any of: stale file (older than 90 s), unreadable or
unparseable file, `status != Healthy`, or certificate TTL under 60 s. It also
prints an "ERROR: agent status is ..." line even when the agent is healthy.

*Target:* the probe checks **freshness only**: the file exists, parses, and its
timestamp is younger than the stale threshold. It does not re-judge `status` or
certificate TTL, because the actor already judged those and acts on them
gracefully. Re-judging in the probe would let two components disagree, and the
disagreement always resolves to a SIGKILL. The probe logic moves into the
`polar-health` library so it can be tested without a process boundary.

When the probe fails twice, the kubelet sends SIGTERM and then SIGKILL after the
grace period. *Target:* SIGTERM now starts the orderly pipeline, so even a probe
kill has a chance to end gracefully. *Today* PID 1 ignores SIGTERM and every probe
kill ends in exit code 137.

## 6. How to read an ending

| Observed exit | Meaning |
|---|---|
| 1, log shows a shutdown reason | Orderly. The reason says which trigger fired. |
| 137, no shutdown reason logged | The kubelet killed the pod: the file went stale or SIGTERM was ignored. In the *Target* design this means the process was wedged. |
| 0 | A bug. No agent should exit 0. |

After verification of the rework, a naturally occurring rejuvenation should end
with exit code 1 and a logged reason, never 137.

## 7. Known gaps this design depends on

- **Cassini session restoration.** After a broker restart, clients get "Session
  not found", the client never signals `Registered` again, and subscriptions are
  lost. Until this is fixed, an agent whose broker restarted is genuinely
  unavailable, and a longer disconnect grace would only prolong the outage. The
  Cassini disconnect grace and the session repair are therefore one paired item.
- **Cassini broker shutdown.** The broker calls `exit(1)` immediately after
  `PrepareForShutdown`, without waiting for a handshake. Tracked separately.
- **Graph-write capability split.** `with_graph_write()` also sets
  `expects_graph`, so health depends on Neo4j whenever an agent writes to it.
  Tracked separately.

## 8. Diagrams

- [`health_rejuvenation.mmd`](sequences/general/health_rejuvenation.mmd): the
  phases, the liveness file, and the probe.
- [`shutdown_pipeline.mmd`](sequences/general/shutdown_pipeline.mmd): the
  shutdown funnel, its triggers, and the watchdog.

Both show the *Target* design. See `sequences/assets/asset_regeneration.md` to
re-render.
