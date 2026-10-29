# local/fork

The fork's Polar platform on a local cluster, written with the library: the operator that keeps agents
alive, the certificate issuer, Cassini, the credentials agent and its observer, and a graph. What a
builder agent (Blitz's, for one) connects to. `../polar/` is the older, nu-init based set and is not
touched.

```sh
polar plan local/fork/plan.dhall                                    # what it would do
polar up   local/fork/plan.dhall --context my-cluster [--with graph]
polar down local/fork/plan.dhall --context my-cluster
```

`plan.dhall` is the whole thing, as stages; `rendered/plan.yaml` is its committed rendering
(`polar render plan.dhall -o rendered`). The unit expressions beside it are what the stages are made of:

| File | What |
|---|---|
| `00-namespace` | `polar` |
| `10-cert-issuer` | The issuer: ServiceAccount, configuration, CA claim, Service, a `Recreate` Deployment. Makes its own CA on first start |
| `20-neo4j` | A graph with a password, no TLS. The optional `graph` stage |
| `31-companions` | ServiceAccounts, the ledger's claim, Cassini's and the observer's Services |
| `30-agents` | `PolarAgent`s: Cassini, the credentials agent, its observer |

Values are in `../values-fork.dhall`. Secrets are made by `polar up`, when absent, with random values:
`cassini-shutdown-token`, `polar-graph-pw` and `neo4j-secret` (the last two share one password).

The platform's agents take their certificates from the issuer directly and skip the escrow bootstrap;
every other agent should be vouched for by the credentials agent (`Issuer.CredentialsAgent`), because its
escrow key needs a ledger entry only that path makes. The issuer's issuance floor is off.
