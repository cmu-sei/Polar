# local/fork

The fork's Polar platform on a local cluster, written with the library: the certificate issuer,
Cassini, the credentials agent and its observer, and a graph. What a builder agent (Blitz's, for one)
connects to. `../polar/` is the older, nu-init based set and is not touched.

| File | What | Waits for |
|---|---|---|
| `00-namespace` | `polar` | |
| `10-cert-issuer` | The issuer: ServiceAccount, configuration, CA claim, Service, a `Recreate` Deployment. Makes its own CA on first start | The operator's CRD and Deployment, if the agents below are to run |
| `20-neo4j` | A graph with a password, no TLS. Optional | |
| `31-companions` | ServiceAccounts, the ledger's claim, Cassini's and the observer's Services | |
| `30-agents` | `PolarAgent`s: Cassini, the credentials agent, its observer | The issuer (they ask it for certificates) |

Order: the operator and its CRD, `00`, the Secrets, `10` (wait until ready), `31`, `30`, `20`. `render.sh`
re-renders `rendered/`. Values are in `../values-fork.dhall`.

Secrets, created by whoever deploys and never rendered: `cassini-shutdown-token` (key `token`),
`polar-graph-pw` (key `secret`, the bare graph password), `neo4j-secret` (key `secret`, `neo4j/<password>`).

The platform's agents take their certificates from the issuer directly and skip the escrow bootstrap;
every other agent should be vouched for by the credentials agent (`Issuer.CredentialsAgent`), because
its escrow key needs a ledger entry only that path makes. The issuer's issuance floor is off.
