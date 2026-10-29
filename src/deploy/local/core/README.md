# local/core

The core of a Polar deployment on a local cluster: the operator, the certificate issuer, Cassini, the
credentials agent and its observer, and the graph. What any platform needs to run atop the graph or
cooperate with its agents.

```sh
polar plan local/core/plan.dhall                                  # what it would do
polar up   local/core/plan.dhall --context my-cluster
polar down local/core/plan.dhall --context my-cluster --core      # shared stages stay unless --core
```

You rarely want this alone. An application's plan includes the core (`Polar.core.core values`), so one
plan stands up an application with everything it needs; the core's stages are `shared`, so applying two
applications' plans applies the core once, and taking one application down leaves the core for the other.

`types/core.dhall` is the core, as a function of one values record; `values.dhall` here is that record for
a local cluster, and `plan.dhall` the core by itself. `rendered/plan.yaml` is the committed rendering
(`polar render plan.dhall -o rendered`).

| Stage | What | Kept alive by |
|---|---|---|
| `operator` | The PolarAgent CRD, and the operator | Kubernetes: the only plain workload (a one-replica Deployment) |
| `issuer` | The `polar` namespace, the secrets, the certificate issuer | the operator (a `ServiceAgent`: no `polar-init`, it is the issuer) |
| `broker` | Cassini | the operator |
| `credentials` | The credentials agent and its observer | the operator |
| `graph` | A graph with a password; development grade (no TLS, so no certificate to go stale) | the operator (a `ServiceAgent`) |

Secrets are made by `polar up`, when absent, with random values: `cassini-shutdown-token`, and
`polar-graph-pw` with `neo4j-secret` (one password in two forms, made together).

Cassini, the credentials agent and its observer take their certificates from the issuer directly and skip
the escrow bootstrap, since they are what it travels through; every other agent is vouched for by the
credentials agent. The issuer's issuance floor is off. The scheduler and the git server it needs are not in
yet.

## Do not run this beside another deployment of the same namespace

The `polar` namespace and the names in it (`cert-issuer`, `polar-db-data`, ...) are fixed by the fork. A
GitOps deployment (Flux, say) that applies the same names will fight this one: it resets a Service's
selector, re-creates what `polar down` removes, and fails on a claim of the same name with a different
class.
