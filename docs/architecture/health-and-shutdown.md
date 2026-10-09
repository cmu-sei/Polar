# Agent Health and Shutdown

Status: **implemented in code and in the catalog (stages S1 to S5 of the rework) and verified
on the dev cluster (stage S6, 2026-10-07).** What was observed: `kubectl delete pod` on the
credentials consumer and on kube-consumer ran the orderly shutdown and exited 1 in 5 and 6 s,
never 137; a forced rejuvenation of kube-observer (threshold above the certificate lifetime, so
it enters the window at once) ran three times in a row, each time logging the reason and exiting 1,
one of them with a 181 s lifetime and a long jitter that the old probe would have killed; and an
agent whose PID 1 was stopped was killed by the probe with 137 after 153 s, as the simulation
bounds it; and jira-processor rejuvenated naturally, after about an hour, through a jitter of
about three minutes with no failed probe, exiting 1 with its reason logged. What was not observed:
the natural rejuvenation of the other four agents, whose previous pods were gone before they were
looked at (see the work item), and broker-restart
recovery, which waits for the Cassini pair.

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
    ├── TcpClientFacade (Cassini client) the stable front; supervises the transport
    │   └── TcpClientActor              the connection; replaced when it dies
    └── CryptoActor                     only with_encryption(); one for the agent's life
```

`polar-agent-builder` wires this. The domain supervisor receives
`InfrastructureEvent::PrepareShutdown` and replies with
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
| Cassini disconnect deadline | 120 s | `POLAR_HEALTH_CASSINI_DEADLINE_SECS` |
| Probe stale threshold | 90 s | `POLAR_HEALTH_STALE_SECS` |

### Startup consistency check

The same check runs before the pod exists: `resolve.nu check` hands the environment each
workload will be given to `polar-config-check`, which calls the function the agent runs, so a
bad combination fails the render naming the workload and the variables. There is one
implementation of these rules, in the health actor's crate.

The actor validates its settings when it starts and refuses to run, with a message
that names the problem, if they contradict each other. A crash-looping agent with a
clear reason in its log is better than one that quietly misbehaves later.

- A numeric variable that is set but is not a whole number is an error. A typo
  used to fall back to the default without a word. A blank value still means the
  default.
- The tick, the graph deadline and the Cassini deadline are at least 1 second.
- `POLAR_HEALTH_EXPIRY_SECS` is positive and `POLAR_HEALTH_REJUVENATION_SECS` is
  greater than it.
- A rejuvenation can finish before the certificate reaches the hard floor. The
  slowest case is a tick to notice the window and check the dependencies, the
  longest jitter, and a tick to act: two ticks plus the jitter window. The room
  between the rejuvenation threshold and the expiry floor must be at least that.
  With the defaults it is 240 s needed and exactly 240 s available, so there is
  nothing to spare; raising the tick or the jitter window means raising the
  threshold.
- The probe's staleness limit (`POLAR_HEALTH_STALE_SECS`, read from the same pod
  environment) exceeds twice the tick, so one slow tick does not make the probe
  call a healthy agent dead.

### Dependency checks

In `WaitingForDeps` the actor checks that each dependency's certificate has enough
life left (`POLAR_HEALTH_DEP_ENDPOINTS`, from the catalog `needs`). The checks run
at the same time, in a task, so a tick never waits on the network and the actor
keeps handling messages meanwhile; the tick chain pauses until the result arrives.
Each check is bounded: 5 s to connect, 5 s for each read or write, and 10 s in
all. Before this, the connect had no limit of its own (about two minutes against a
host that drops packets) and the endpoints were checked one after another, inside
the message handler. `polar-init` uses the same check when it waits for dependency
certificates, so it gains the same limits.

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
  `POLAR_HEALTH_CASSINI_DEADLINE_SECS` (default 120 s), timed from startup until the
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

One funnel, `begin_shutdown(reason)`, with every trigger feeding it. The reason is
logged each time. The decisions live in `polar-agent-builder/src/shutdown.rs`, as
plain functions that can be tested without starting an actor.

| Trigger | Before | Now |
|---|---|---|
| Health verdict: rejuvenation reached | orderly | orderly |
| Health verdict: hard deadline (cert, graph, Cassini) | abrupt (actor fails) | orderly |
| HealthCheckActor dies unexpectedly (panic, bug) | abrupt | orderly, logged as a fault in the health actor |
| Router / CryptoActor dies unexpectedly | abrupt | orderly |
| Cassini connection (the transport) dies | the client died with it and was replaced, and every holder was told | the client front brings a new transport up with backoff, holding publishes meanwhile; nothing else is told |
| Cassini client front ends (error, panic or clean stop) | abrupt, or replaced | orderly |
| SIGTERM or SIGINT | ignored by PID 1 | orderly |
| `kubectl delete`, rollout, node drain | SIGKILL after the grace period | orderly, via SIGTERM (grace period: stage S5) |

Design decisions:

- **Verdict messages, not death.** The HealthCheckActor reports its decision with
  an explicit message, delivered through a port the supervisor gives it, and never
  uses its own termination as the signal. Its *unexpected* death is still treated
  as "something is very wrong" and triggers shutdown, but it is the exception path,
  not the protocol.
- **One pipeline.** `PrepareShutdown` → domain supervisor drains and stops its own
  children → `ProceedWithShutdown` → infrastructure children stop → the domain
  supervisor exits non-zero when the infrastructure supervisor ends. Every trigger
  takes the same steps, and the domain supervisor needs no change to get it.
- **Shutdown is idempotent.** The first trigger starts it; a second while it is
  under way (SIGTERM during a rejuvenation drain, Kubernetes growing impatient) is
  logged and changes nothing. The domain supervisor is told once.
- **Children ending during a shutdown are expected.** Once it has begun, the
  children are being stopped on purpose, so nothing they do starts anything:
  no second `PrepareShutdown`. (Stopping the health actor
  used to start a second shutdown.)
- **In-process watchdog.** When the shutdown begins, a detached task starts a clock
  for the shutdown budget. If the process is still alive when it runs out, the
  domain supervisor never finished, and the process exits 1. Previously the
  supervisor waited indefinitely and only the Kubernetes probe could end a hung
  shutdown. With the watchdog the probe covers only what it should: a process in
  which nothing can run. The task does not depend on the supervisor actor still
  being alive.
- **The shutdown budget.** 20 s by default. An agent whose drain takes longer
  declares it in code with `AgentBuilder::with_shutdown_budget`. The deployment's
  `POLAR_SHUTDOWN_BUDGET_SECS` wins when it is set, because the same value sizes the
  pod's termination grace period; a value that is not a whole number of seconds is
  an error at startup.
- **Bounded steps.** Stopping the infrastructure children is bounded at 10 s.
  Safety comes from the bounds, never from assuming the children are still alive.
- **Signals.** SIGTERM and SIGINT are turned into a shutdown request by a listener
  the builder starts. If it cannot register, that is logged as an error, because
  the pod will not shut down in an orderly way when Kubernetes asks.

### Termination grace

SIGTERM handling only helps if the kubelet waits long enough. The Kubernetes
default grace period is 30 s, and several agents drain for longer, so the pod's
`terminationGracePeriodSeconds` is set from the agent's shutdown budget: the budget plus
a 10 s margin. Kubernetes sends SIGKILL when it ends, and the agent's own watchdog exits
at the budget, so the pod has to be allowed to live longer than that.

The number has one home. The catalog (`infra/config/kinds.nuon`, `shutdown`) holds the
default and each workload overrides its budget; the resolver derives
`POLAR_SHUTDOWN_BUDGET_SECS` and the grace period from it, and the grace period cannot
be set on its own. The agent's code declares the least it needs (`with_shutdown_budget`),
and the builder refuses to start if the deployment gives less. So a deployment can raise a
budget freely, and lowering what an agent needs is a code change someone has to review.
`resolve.nu check` also rejects, at render time, a grace period that does not exceed the
budget, and settings in which a rejuvenation could not finish before the certificate's
hard floor.

| Agent | Budget | Grace period | What it is made of |
|---|---|---|---|
| credentials consumer | 50 s | 60 s | 5 s drain window, ledger stop, projection stop (up to 30 s) |
| credentials observer | 40 s | 50 s | HTTP drain, up to 30 s |
| kube consumer | 45 s | 55 s | 5 s drain window, graph controller stop (up to 30 s) |
| kube observer | 40 s | 50 s | three watchers stopped in turn, 10 s each |
| jira observer | 40 s | 50 s | each observer stopped in turn, 10 s each |
| jira processor | 40 s | 50 s | graph controller stop, up to 30 s |

The four catalog workloads get these from the catalog. The two credentials agents are still
hand-written charts and carry the same numbers directly until they move to the catalog.

## 5. The Kubernetes liveness probe

`polar-healthcheck` is the container's liveness exec probe. The catalog sets it for
every agent (`infra/config/kinds.nuon`, `probe`): `initialDelaySeconds` 150,
`periodSeconds` 30, `failureThreshold` 2, `timeoutSeconds` 5.

It is a dead-man's switch and checks one thing: **the health file exists, parses,
and is no older than `POLAR_HEALTH_STALE_SECS` (90 s).** It fails otherwise. It does
not judge the agent's `status`, its connections, or its certificates. Those are the
actor's decisions, made earlier and acted on in an orderly way, and judging them a
second time here could only make the two disagree. Every disagreement used to end
the same way, with the kubelet killing a pod the actor was in the middle of shutting
down. The probe prints what the agent reports about itself (status, phase,
connections), for whoever reads the pod's events, and does not act on it. A
malformed `POLAR_HEALTH_STALE_SECS` is an error, as it is for the actor, which reads
the same variable.

The logic is `polar_health::probe::judge`, a pure function of the file's contents and
the time, so it can be tested without a process.

### What the probe is for

With every tick in every phase writing the file, and the drain after a verdict
bounded by the shutdown budget (always shorter than the staleness limit), the probe
should never fire on a working agent. It fires when nothing in the process can run:

- the process is deadlocked or wedged, so the actor stops writing; or
- the actor has died without anyone noticing.

How fast it acts then is a bound, not a hope. The file turns stale 91 s after the
last write, the first probe to see it fails (at most one period later), and the
second consecutive failure, a period after that, kills the pod: at most
91 + 30 + 30 = 151 s after the last write, never before 91 s.

### How this is tested

`simulation.rs` runs the real health machine next to a model of the kubelet with
the catalog's probe settings, in virtual time. It sweeps every jitter value (0 to
179 s) against every phase of the actor's ticks relative to the kubelet's probes
(5,400 pods) and checks:

- an agent rejuvenating, and then draining after its verdict, is never killed;
- the slowest rejuvenation in the sweep still gives its verdict before the
  certificate reaches the hard floor, with 1 s to spare;
- an agent draining after an *unhealthy* verdict is not killed either;
- an actor that stops writing is killed, within the bound above, whatever phase it
  was in; and one that stops after its verdict ends itself first;
- a healthy agent is never killed over an hour.

Putting the two old defects back (no file writes during the rejuvenation phases;
the probe judging status) makes those tests fail, the first at a jitter of 91 s,
which is the kill seen on the cluster.

## 6. How to read an ending

| Observed exit | Meaning |
|---|---|
| 1, log shows a shutdown reason | Orderly. The reason says which trigger fired. |
| 137, no shutdown reason logged | The kubelet killed the pod. The actor writes the file every tick and SIGTERM is handled, so this now means the process was wedged, or the pod's grace period is shorter than the agent's shutdown budget (until stage S5 sets it). |
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

See `sequences/assets/asset_regeneration.md` to re-render.

## The Cassini broker

The broker is not built with the agent builder, and has its own copy of the same ideas.

- **What starts a shutdown.** The health actor ending or failing, SIGTERM or SIGINT, or a
  `PrepareForShutdown` with the shutdown token. Each begins the same five phases (stop accepting
  connections, drain sessions, flush topic queues, terminate subscribers, terminate listeners),
  after which the broker stops and the process exits.
- **The budget.** `CASSINI_SHUTDOWN_BUDGET_SECS` (default 20) is what the whole sequence has, and
  each phase gets a fifth of it. A phase that overruns has its manager stopped and the sequence
  goes on.
- **The watchdog.** When the sequence begins a detached task sleeps for the budget and then exits
  the process with 1. A sequence that finishes in time stops it. This is what keeps a stuck
  manager from outliving the pod's grace period.
- **The grace period.** The Cassini chart sets `terminationGracePeriodSeconds` to the budget plus
  a 10 second margin, from the same two values that set the environment variable, so they cannot
  disagree. Before this the pod had Kubernetes' default 30 seconds, no signal handler, and a
  sequence that could take 150.
