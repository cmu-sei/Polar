

# Kubernetes Deployment State Temporal Data Model

**Purpose:** Model Kubernetes workload state (past and present) in a way that supports historical reconstruction.

---

# 1. Design Goals

### 1.1 Determinism

Given the same input events, the graph must reconstruct identical historical state.

### 1.2 Temporal Integrity

State must never be overwritten. All lifecycle transitions are append-only.

### 1.3 Causality Preservation

We must be able to trace:

Commit → Artifact → Deployment Revision → ReplicaSet → Pod → PodState

### 1.4 Metric Computability

The model must allow answering:

* When did a deployment revision first become available?
* When did pods become Ready?
* How long was a rollout in progress?
* What was the system state at time T?
* How many pods were affected by a change?

---

# 2. Conceptual Model

There are three conceptual layers:

1. **Identity layer** — stable logical objects (Deployment, Pod, etc.)
2. **Revision layer** — versioned intent (DeploymentRevision, ReplicaSet revision)
3. **State layer** — temporal state transitions (PodState, DeploymentState)

Never mix identity and state.

---

# 3. Core Entities

## 3.1 Workload Identity Nodes

These represent durable logical objects.

```
(:Cluster)
(:Namespace)
(:Deployment {uid, name})
(:ReplicaSet {uid, name})
(:Pod {uid, name})
(:Container {name})
```

Key properties:

* `uid` (Kubernetes UID)
* `name`
* `namespace`
* `created_at` (from metadata.creationTimestamp)

These nodes are stable identities.
They do not contain mutable state fields.

---

## 3.2 Revision Nodes

You must explicitly model revision boundaries.

```
(:DeploymentRevision {
  revision_number,
  template_hash,
  created_at
})

(:ReplicaSetRevision {
  template_hash,
  created_at
})
```

Relationships:

```
(Deployment)-[:HAS_REVISION]->(DeploymentRevision)
(DeploymentRevision)-[:REALIZED_AS]->(ReplicaSet)
(ReplicaSet)-[:OWNS]->(Pod)
```

If you do not model revisions, you cannot reason about rollout boundaries.

---

## 3.3 Artifact Layer (Required for Lead Time)

```
(:Commit {sha, authored_at})
(:Image {digest})
(:Artifact {digest})
```

Relationships:

```
(Commit)-[:PRODUCES]->(Artifact)
(Artifact)-[:DEPLOYED_AS]->(DeploymentRevision)
```

This is what makes lead time computable.

---

# 4. Temporal State Model

This is the critical part.

You do not store mutable fields on identity nodes.

Instead, you create state nodes.

---

## 4.1 Pod State

```
(:PodState {
  phase,                // Pending, Running, Succeeded, Failed
  ready,                // boolean
  reason,
  valid_from,
  valid_to,             // null if current
  observed_at
})
```

Relationships:

```
(Pod)-[:TRANSITIONED_TO {at}]->(PodState)
```

Rules:

* `valid_from` = transition time
* `valid_to` = next transition time
* Never update a PodState after creation
* Close previous state by setting `valid_to`

Deletion is modeled as:

```
(Pod)-[:TRANSITIONED_TO {at}]->(:PodState {phase: "Deleted"})
```

Not as a flag.

---

## 4.2 Deployment State

Deployments have higher-level conditions:

```
(:DeploymentState {
  available_replicas,
  updated_replicas,
  unavailable_replicas,
  progressing_condition,
  available_condition,
  valid_from,
  valid_to
})
```

Relationships:

```
(Deployment)-[:TRANSITIONED_TO {at}]->(DeploymentState)
```

This lets you compute:

* Rollout start
* Rollout completion
* Availability regressions

---

## 4.3 ReplicaSet State

Optional but strongly recommended for precision.

```
(:ReplicaSetState {
  replicas,
  ready_replicas,
  available_replicas,
  valid_from,
  valid_to
})
```

---

# 5. Event Ingestion Model

Your watcher receives:

* ADDED
* MODIFIED
* DELETED

You do NOT blindly create new state on every MODIFIED.

You must compute:

Did any state-relevant field change?

If no → ignore.

If yes:

* Close previous state (`valid_to = transition_time`)
* Create new state node

Transition time precedence:

1. Kubernetes condition lastTransitionTime
2. metadata.deletionTimestamp
3. metadata.creationTimestamp
4. Fallback to watch event timestamp
5. `observed_at` only as last resort

Never treat scrape time as canonical transition time.

---

# 6. Query Patterns Enabled

### 6.1 First Ready Pod of Revision

```
MATCH (rev:DeploymentRevision)-[:REALIZED_AS]->(:ReplicaSet)-[:OWNS]->(p:Pod)
MATCH (p)-[:TRANSITIONED_TO]->(s:PodState)
WHERE s.ready = true
RETURN min(s.valid_from)
```

---

### 6.2 Rollout Completion Time

```
MATCH (d:Deployment)-[:TRANSITIONED_TO]->(s:DeploymentState)
WHERE s.available_replicas = s.updated_replicas
RETURN min(s.valid_from)
```

---

### 6.3 State At Time T

```
MATCH (p:Pod)-[:TRANSITIONED_TO]->(s:PodState)
WHERE s.valid_from <= T AND (s.valid_to IS NULL OR s.valid_to > T)
RETURN s
```

This is why `valid_to` matters.

---

# 7. Handling Deletions Correctly

Deletion is not:

```
pod.deleted_at = t
```

Deletion is:

```
(Pod)-[:TRANSITIONED_TO]->(:PodState {phase: "Deleted"})
```

And the previous state’s `valid_to` is set to t.

This preserves lifecycle boundaries.

---

# 8. Why This Model Works for Metrics

## Lead Time

```
lead_time = first_ready_time - commit.authored_at
```

Both values are explicitly recorded.

---

## Blast Radius

Blast radius of a revision:

```
COUNT pods owned by ReplicaSet of that revision
```

Blast radius over time:

```
COUNT pods where state overlaps with interval
```

Requires valid_from / valid_to windows.

---

## Risk Gradient

Risk gradient is not static.

You compute:

* Exposure duration
* Number of dependent services
* Number of nodes affected

All require temporal and dependency edges.

---

# 9. Storage Discipline Rules

1. Identity nodes are immutable except metadata.
2. State nodes are immutable once created.
3. Every transition closes the previous state.
4. Never overwrite a status field on identity nodes.
5. Never compute metrics off observer polling timestamps.

If you violate any of these, metrics become untrustworthy.

Here’s the clean narrative of how you arrived here — stripped of implementation noise.

---

You started with a fairly straightforward projection goal: ingest Kubernetes watch events and materialize resources into a graph. Initially, the model was snapshot-oriented — each event resulted in a node upsert, maybe some relationships, but fundamentally you were representing *current state*.

Then a problem emerged.

If you simply overwrite nodes on change, you destroy temporal information. You lose the ability to reason about:

* how long a pod was not ready
* how many times it oscillated
* whether restarts cluster around certain rollouts
* how control-plane instability propagates

That pushed you toward modeling *state transitions explicitly* instead of just storing the latest snapshot.

The first step was introducing a stable identity node per Kubernetes object:

```
(:Pod {uid})
```

Then separating mutable state into distinct state nodes:

```
(:PodState {…})
```

And linking them:

```
(:Pod)-[:HAS_STATE]->(:PodState)
(:PodState)-[:TRANSITIONED_TO]->(:PodState)
(:Pod)-[:CURRENT_STATE]->(:PodState)
```

That decision fundamentally changed the system from “state mirroring” to “temporal modeling.”

From there, several architectural consequences followed:

1. You needed a deterministic way to decide when a new state should be emitted → enter semantic signatures and a projection cache.

2. You needed stable state node IDs → derived from semantic fingerprints, not resourceVersion.

3. You needed a new graph primitive (`ReplaceEdge`) to enforce cardinality for CURRENT_STATE.

4. You realized Pods aren’t unique — many Kubernetes objects have meaningful state transitions. So you needed a generalized projection pipeline rather than ad hoc logic.

At that point the scope became clear:

You are no longer just indexing Kubernetes.
You are building a temporal state graph capable of supporting higher-order analysis — risk gradients, control-plane health evolution, lifecycle duration metrics, propagation modeling.

And that leads to the uncomfortable realization:

If the graph doesn’t support chained state modeling generically, it cannot support analysis robustly. Snapshot-only modeling collapses too much signal.

So yes — it expanded. Because once you commit to temporal correctness, partial modeling becomes analytically inconsistent.

That’s how you got here.

You started building a projection layer.
You discovered you were actually building a temporal control-plane model.


If you want a coherent temporal model, you don’t get to special-case Pods and pretend the rest of the control plane is static.

But that does **not** mean hand-rolling bespoke projection structs forever.

The better framing is:

> Do I want a *generic state projection framework*, with resource-specific extractors?

Those are very different levels of effort.

---

## First: Be Clear What “Changes State” Means

Not every Kubernetes object deserves chained temporal modeling.

There are three categories:

### 1. Ephemeral workload state (high churn, high signal)

* Pods
* Containers (embedded in Pod status)
* Jobs
* Nodes

These benefit heavily from chained state modeling.

You care about:

* Scheduling latency
* Crash loops
* readiness transitions
* phase changes

These absolutely justify projection structs.

---

### 2. Declarative desired-state objects (low churn)

* Deployments
* StatefulSets
* ReplicaSets
* PVCs

These mostly change spec or status occasionally.

You may not need fine-grained state chains unless you're modeling rollout progression or drift.

---

### 3. Structural objects (almost static)

* Services
* ConfigMaps
* Secrets (ignoring rotation)
* Namespaces

These rarely change in a way that benefits from chained modeling.

You can snapshot-update these instead of chaining.

---

So no — you don't blindly apply this to “everything.”

You apply it where temporal transitions matter analytically.

---

# The Real Abstraction You Want

You want something like this:

```rust
trait StatefulProjection {
    type Projection: Serialize;

    fn project(&self) -> Self::Projection;
}
```

Then your generic signature pipeline works for anything implementing that trait.

---

Example for Pod:

```rust
impl StatefulProjection for Pod {
    type Projection = PodStateProjection;

    fn project(&self) -> Self::Projection {
        project_pod_state(self)
    }
}
```

Then your generic state logic becomes:

```rust
fn compute_signature<T: StatefulProjection>(resource: &T) -> String {
    let projection = resource.project();
    let payload = serde_json::to_string(&projection).unwrap();
    hash_signature(&payload)
}
```

Now you’ve separated:

* Graph mechanics (generic)
* Signature hashing (generic)
* Resource-specific semantic extraction (minimal per-type code)

That scales.

---

# Containers?

Containers are tricky.

They are not first-class Kubernetes objects.

They exist inside Pod status.

If you want container-level state chains, you:

* Treat `(pod_uid, container_name)` as identity
* Project container readiness, restart_count, state.waiting.reason, etc.

That’s fine. Same pattern.

---

# Deployments?

Do you need chained Deployment states?

Only if you care about:

* rollout progression
* replica convergence
* unavailable replicas
* paused status
* generation vs observedGeneration drift

If yes → projection struct.

If not → upsert-only node.

---

# This Is the Core Design Decision

If your system aims to compute:

* Risk gradients
* Failure propagation
* Mean time in degraded state
* Control plane health evolution

Then yes — you need consistent temporal modeling.

But you don’t need it for every resource.

You need it for every resource whose **state transitions have analytic meaning**.

That’s the filter.

---

# The Mistake to Avoid

Do not:

* Hash entire objects
* Blindly chain everything
* Model declarative spec edits as temporal state unless you explicitly want drift tracking

You’ll drown in noise.

---

# The Real Architecture

You’re building:

Stable identity nodes
Optional chained state nodes
Per-resource projection logic
Generic state transition engine

That is scalable.

What you do *not* want is per-resource bespoke graph logic. Only per-resource semantic extraction.

---

# Brutal Truth

Yes, if you want a principled temporal graph, you need to be explicit about semantics per resource.

But no, you don’t need to explode your codebase.

You need:

* One generic transition pipeline
* Thin resource-specific projection adapters

That’s it.
