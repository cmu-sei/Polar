# polar

Administer a Polar deployment from one place.

```sh
polar plan  platform.yaml app.yaml                       # what would happen; needs no cluster
polar up    platform.yaml app.yaml --context my-cluster  # do it, stage by stage, waiting for each
polar status --context my-cluster                          # every agent, and why one is not running
polar down  platform.yaml app.yaml --context my-cluster  # backwards; never the CRD
polar render plan.dhall -o out/                            # Dhall to YAML
```

The cluster is always named, with `--context` or `POLAR_CONTEXT`. The kubeconfig's current context is
never used: a stale default must not be able to send a plan somewhere else.

## What it is, and is not

The operator starts agents and keeps one alive. The scheduler configures them at runtime. Agents do their
own work, and applications on the graph do the business logic. This is the tool a person uses to put the
rest in place and see how it is doing.

It is a **generic executor of a plan** that the consumer declares in Dhall. It knows nothing about what
is deployed, and it has no environments, no registry, no image builds and no GitOps. Order, environments
and secrets are the consumer's; the plan is where they are written down. If a feature would need to know
about a particular deployment, it does not belong here.

## A plan

Written with the library (`src/deploy/types/plan.dhall`), rendered to one YAML file that is committed, so
the exact thing applied is in the repository. A worked example is `src/deploy/examples/plan.dhall`.

```yaml
name: polar-platform
stages:
  - name: operator
    installCrd: true                  # the PolarAgent CRD, which this tool carries
    objects: [ ... ]                  # applied server-side, so running it again changes nothing
    waits:
      - { apiVersion: apps/v1, kind: Deployment, namespace: polar-operator, name: polar-operator,
          field: status.readyReplicas, equals: "1", timeoutSecs: 300 }
  - name: issuer
    secrets:                          # made only when absent; values never printed
      - { namespace: polar, name: cassini-shutdown-token, keys: { token: { random: 24 } } }
      - { namespace: polar, name: polar-graph-pw, group: graph, keys: { secret: { random: 16, id: password } } }
      - { namespace: polar, name: neo4j-secret,   group: graph, keys: { secret: { template: "neo4j/{password}" } } }
    objects: [ ... ]
  - name: graph
    optional: true                    # only with `--with graph`
    objects: [ ... ]
```

Within a stage the order is always the same, so a plan reads the way it runs: the CRD, the namespaces
(everything else lives in one), the secrets, the other objects, then the waits. The next stage starts
when the waits have passed.

| | |
|---|---|
| **Objects** | Any Kubernetes object, or a `PolarAgent`. Applied server-side with field manager `polar` |
| **Secrets** | Made only when absent. A key's value is `{literal}`, `{random: bytes, id?}` (hex), `{template: "..{id}.."}` or `{copy: {namespace, name, key}}`. Secrets in one `group` are made together, sharing the random values they name, or not at all: a group that is partly there is an error that names both halves. Nothing prints a value |
| **Waits** | Until `field` (a dotted path) of an object equals `equals`. When a wait on a `PolarAgent` times out, the tool prints what the operator recorded: the phase, its message, and how the last instance ended and what it said |
| **Optional** | A stage with `optional: true` runs only with `--with <stage>`; `--only <stage>` runs just that one |

`polar up --dry-run` has the API server judge each change without making it. Objects in a namespace the
plan has yet to create are reported as such, not failed, since a dry run creates nothing.

`polar plan` judges a plan without a cluster and names every problem at once: an object with no
identity, a duplicate, a template that uses an id nothing defines, a wait on something the plan never
creates.

## Several plans

`polar up platform.yaml app.yaml` runs them in the order given; `down` takes them down in the reverse
order. `--with` and `--only` apply to whichever plan has a stage of that name; a name no plan has is an
error.

## Building

```sh
cd src/agents && cargo build --release -p polar-cli      # target/release/polar
```

Needs `dhall-to-yaml` on the PATH only for `render` and for passing a `.dhall` file where a plan goes.
