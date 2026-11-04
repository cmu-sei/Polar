<!--
  ACE Tool

  Copyright 2026 Carnegie Mellon University.

  NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
  FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
  EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
  PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
  MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
  TRADEMARK, OR COPYRIGHT INFRINGEMENT.

  Licensed under a MIT (SEI)-style license, please see license.txt or contact
  permission@sei.cmu.edu for full terms.

  [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
  distribution.  Please see Copyright notice for non-US Government use and distribution.

  This Software includes and/or makes use of Third-Party Software each subject to its own license.

  DM26-0670
-->

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
| `git` | The configuration git server (mTLS), which makes the `polar-schedules` repository and seeds it from its image | the operator |
| `scheduler` | The scheduler's processor (records schedules in the graph) and its observer (watches the repository). Started after the git server: the observer ends if the server is not there | the operator |

Secrets are made by `polar up`, when absent, with random values: `cassini-shutdown-token`, and
`polar-graph-pw` with `neo4j-secret` (one password in two forms, made together).

Cassini, the credentials agent and its observer take their certificates from the issuer directly and skip
the escrow bootstrap, since they are what it travels through; every other agent is vouched for by the
credentials agent, the git server and the scheduler among them. The issuer's issuance floor is off. The git
server answers only to the identities its access list names: the scheduler's observer, to read, which is why
that agent's ServiceAccount is named `polar-scheduler-observer`.

## Do not run this beside another deployment of the same namespace

The `polar` namespace and the names in it (`cert-issuer`, `polar-db-data`, ...) are fixed by the fork. A
GitOps deployment (Flux, say) that applies the same names will fight this one: it resets a Service's
selector, re-creates what `polar down` removes, and fails on a claim of the same name with a different
class.
