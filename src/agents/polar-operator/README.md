# polar-operator

Keeps services that follow the Polar agent contract alive on Kubernetes (one instance of
each deployment, never two, every ending recorded and explained) and tells the rest of
Polar about it, so that each agent has a canonical node in the graph that is independent
of where it is deployed.

Status: a rough prototype. The operator runs from a laptop against the cluster in
`KUBECONFIG`, with that kubeconfig's permissions; it has no image and no RBAC of its own yet.

| Crate | What it is |
|---|---|
| `common/` (`polar-operator-common`) | The `PolarAgent` resource, shared by the operator and the kube agents |
| `operator/` (`polar-operator`) | The controller: keeps agents alive, publishes lifecycle events |
| `processor/` (`agent-processor`) | Projects lifecycle events into the graph: agents, deployments, instances |

The kube observer watches `PolarAgent` resources like any other, and the kube consumer ties
each one to the agent it deploys and to the pods it owns.

## Where the operator stops and the scheduler begins

The operator **starts processes**: which agents exist, from which image, as which identity,
with which capabilities and how long to drain. Anything that needs a new pod is the
operator's. The scheduler **configures them at runtime**, once an instance has started,
initialized and proven its identity by registering with Cassini under its certificate:
dynamic variables, runtime sections, commands. The agent's own declarations (in
`polar-agent-builder`) draw the line, because they say what it accepts while running.

So the operator never delivers configuration, and the scheduler never starts or stops a
pod. They meet in the graph: `InstanceStarted` records the pod and the identity it runs
as, and that identity is what the scheduler sees register, so a configuration outcome can
be tied to the instance it reached.

## The contract

What the operator relies on, and what `polar-agent-builder` gives every agent:

- an instance never ends on purpose with 0: every ending asks for a replacement;
- it says why it ended in `/dev/termination-log` (without one, its last log lines are used);
- once sent SIGTERM, it drains within `shutdownBudgetSecs`.

Anything that does this can be a `PolarAgent`; `demo/agents.yaml` shows two busybox
scripts that do.

## What the operator does

| Rule | Where |
|---|---|
| A new instance starts only when no pod of the agent exists, terminating ones included | `lifecycle.rs` rule 1 |
| Each ending is recorded once in the status (kind, exit code, the instance's own reason, how long it ran), then its pod is removed | rule 2 |
| An ending within 30 s of the start is a failure; failures in a row back off (2 s doubling to 5 min). An ending after a long run (a rejuvenation) is replaced at once | rule 3 |
| 5 failures in a row hold the agent in `CrashLoop`; changing the spec releases it | rule 4 |
| A changed spec stops the running instance (SIGTERM, then its grace period) and starts the new one once it is gone | rule 5 |
| Every pod is hardened: non-root, read-only root, no capabilities, RuntimeDefault seccomp, no service-account token, a writable `/tmp` only | `pod.rs` |
| Every agent gets its own ServiceAccount (its identity) unless the spec names one | `controller.rs` |
| `POLAR_CAPABILITIES`, `POLAR_SHUTDOWN_BUDGET_SECS` and `POLAR_AGENT_NAME` are given to every instance; the grace period is the budget plus 10 s | `pod.rs` |

The decisions are a pure function (`lifecycle::decide`) tested without a cluster. The
operator's memory (backoff, failure count, instances started) lives in the status, so an
operator restart picks up where it left off.

## Lifecycle events

With `BROKER_ADDR` set, the operator publishes an `AgentLifecycleEvent` (rkyv, defined in
`lib/src/events.rs`) on `polar.agents.lifecycle` for each fact it observes. Every event names
the agent by the name its code declares (`spec.agent`, else the resource's name) and the
deployment as a `DeploymentRef`; instances are `InstanceRef`s. Kubernetes is one variant of
each, so another target adds variants, not fields.

| Event | When |
|---|---|
| `DeploymentDeclared` | The spec changes, and once per deployment each time the operator starts or reconnects to the broker; the live instance is re-announced with it |
| `DeploymentPhaseChanged` | The phase moves (`Starting`, `Running`, `Ending`, `BackingOff`, `CrashLoop`, ...) |
| `InstanceStarted` | An instance was created: which one, its number, the spec hash |
| `InstanceEnded` | An instance ended: kind, exit code, the target's reason, the instance's own message, how long it ran |

Events are best effort: keeping agents alive never waits on the broker, and the next
declaration repairs whatever the graph missed. Without `BROKER_ADDR`, nothing is published.

## In the graph

Three levels, and only the first is canonical:

| Level | Node | Holds |
|---|---|---|
| Agent | `(:PolarAgent {name})` | What the code is: its name, for now. Nothing about any target |
| Deployment | `(:KubernetesPolarAgent {uid, cluster_uid})` | Image, spec hash, drain budget, identity; its phase history |
| Instance | `(:Pod)` | One run, and how it ended |

```text
(:KubernetesPolarAgent)-[:DEPLOYS]->(:PolarAgent {name})                 kube consumer and processor
(:KubernetesPolarAgent)-[:IN_NAMESPACE]->(:Namespace)                    kube consumer
(:KubernetesPolarAgent)-[:OWNS]->(:Pod)                                  kube consumer, from owner references
(:KubernetesPolarAgent)-[:STARTED_INSTANCE {number, spec_hash, started_at}]->(:Pod)
(:KubernetesPolarAgent)-[:TRANSITIONED_TO]->(:KubernetesPolarAgentState {phase, message, valid_from})
(:KubernetesPolarAgent)-[:INSTANCE_ENDED]->(:AgentInstanceEnding)<-[:ENDED_WITH]-(:Pod)
```

One agent deployed in two places is one `PolarAgent` with two deployments. The agent
processor names a target's nodes in one place (`deployment_key`, `deployment_state_key`,
`instance_key`); for Kubernetes they are the kube consumer's own nodes, so the two writers
meet. The phase history is the processor's alone; the kube consumer records only the
resource's deletion. As in the kube consumer, the current phase is the state with the latest
`valid_from`.

**Capabilities are not in the graph**, as nodes or as properties. That a deployment is
configured to give an agent a capability asserts nothing about whether it is present, and an
agent built on `polar-agent-builder` judges its runtime state before it initializes. A
misconfigured agent is one that refuses to start, so the question is asked of its endings:

```cypher
// Agents that ended soon after starting, and why
MATCH (a:PolarAgent)<-[:DEPLOYS]-(d)-[:INSTANCE_ENDED]->(e:AgentInstanceEnding)
WHERE e.ran_secs < 30 AND e.exit_code <> 0
RETURN a.name, d.name, e.exit_code, e.message, count(*) AS times
```

## Running it

Only the operator, with no broker (lifecycle only):

```sh
cd src/agents
cargo run -p polar-operator -- crd | kubectl apply -f -
cargo run -p polar-operator -- run            # leave running
kubectl apply -f polar-operator/demo/agents.yaml
kubectl get pagent -n polar-demo -w
```

The whole loop needs the broker and Neo4j, then the agent processor, the kube consumer and
observer, and the operator, all with the usual Cassini client variables (`BROKER_ADDR`,
`CASSINI_SERVER_NAME`, `TLS_CLIENT_CERT`, `TLS_CLIENT_KEY`, `TLS_CA_CERT`); the processor and
the kube consumer also need `GRAPH_ENDPOINT`, `GRAPH_USER`, `GRAPH_PASSWORD` and `GRAPH_DB`.

To see a rollout, change the `steady` agent's spec. To release `refuses` from its crash
loop, give it the capability it asks for:

```sh
kubectl patch pagent steady -n polar-demo --type=json \
  -p '[{"op":"replace","path":"/spec/env/1/value","value":"hello again"}]'
kubectl patch pagent refuses -n polar-demo --type=merge \
  -p '{"spec":{"needs":["cassini","graph-write"]}}'
```

Clean up with `kubectl delete ns polar-demo` and
`kubectl delete crd polaragents.polar.sei.cmu.edu`.

## Not yet

- Certificates: `polar-init` as an init container, so an instance gets its mTLS identity.
  Until then, the scheduler's half of the boundary above has nothing to attest.
- A deleted `PolarAgent` is recorded by the kube consumer (a `Deleted` state on the
  deployment), but the operator sends no event for it: that needs a finalizer.
- `spec.agent` is set by hand; for a builder agent the operator could read it from the
  image's `--scaffold`, along with the rest of what the agent declares about itself.
- For builder agents, a pre-flight before a rollout: the new image's `--check` on its config.
- Capabilities that become RBAC and NetworkPolicy, as the catalog's facets do.
- Pod fields beyond the contract: resources, volumes, node selection.
- In a cluster, the kube observer's ClusterRole must allow `get`, `list` and `watch` on
  `polaragents.polar.sei.cmu.edu`.
- An instance stopped by hand (`kubectl delete pod`) within 30 s of starting counts as a
  failure.
- The agent processor has no health check actor yet.
