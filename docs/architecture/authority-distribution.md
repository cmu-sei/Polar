# Authority distribution: policy and permissions pushed to the leaves

Status, 2026-10-12: design, decided in part with David (marked) and open in part (the questions at the end).
Nothing in it is built. It supersedes the placement questions in `credentials-policy.md` and the "policy for other
receivers" note in `git-controller.md`.

## The principle (David, 2026-10-12)

The credentials agent is the **ultimate decision authority** for policy and permissions, and it is designed to
**push decisions out** to be consumed at the endpoints. It is not the runtime extension of the agents that need
them. Every agent evaluates, locally, whatever concerns it: that is the point of a distributed system, and a
decision that does not change should never cost a round trip to the credentials agent. The early diagrams
(`sequences/general/policy_update_enforcement.mmd`) already had a policy update event delivered to the leaves.

So: policy, permissions and configuration are delegated to the leaf nodes. Each leaf hosts the evaluator. When an
authoritative update is accepted, an **event is raised** to every consumer of it, so the effect is immediate.

## Decided

1. **Groups are principal data, not rules** (option A). They live with `PermissionsActor`, the sole authoritative
   source of what a principal holds, as another signed input of its own. The policy document carries rules only.
   `polar-policy`'s invariant stands: a principal's attributes come only from `PermissionsActor`, and no caller
   supplies them. Who may assign people to groups can therefore be a different authority from who may write rules.
2. **Every receiver hosts the evaluator** (option i). The builder gains a `PermissionsActor` and a `PolicyActor`,
   next to the `ConfigurationActor` and `CommandActor` it already has. Policy rules and principal data are
   delivered to every agent, and each agent decides for itself whether a signer may change it or run a command on it.
3. **An event signal for updates.** When the credentials agent accepts a policy or permissions update, it raises an
   event for the consumers, handled everywhere the update needs to be received.

## Where things are decided

There are two evaluation sites, running the same machinery:

| | Decides | On what | Resource data comes from |
|---|---|---|---|
| **The credentials agent** | who may have a secret it holds; admin actions | its ledger | the ledger (`owner`, labels, a freeze) |
| **Every other agent** (the builder) | may this signer change a section of mine, change my configuration, or run this command on me | what the agent was sent | the agent itself: its own sections, variables and commands are the resources, built from its declarations |

The second needs no external resource data: the receiving agent knows its own `Section::"git-repo-controller/repositories"`.
The first has resource data that the ledger owns, and a change to it (a secret frozen) is a ledger event raised by
an admin action, not an annotation in a document. It matters only where the secrets are, at the credentials agent.

## What is pushed

Three kinds of authoritative data, each replaced whole and versioned:

- **Policy**: the Cedar rules. Authored with `c:update-policy` (exists). A `policy` document.
- **Principal data**: group membership and supplemental permissions. A `permissions` document. The capability to
  author it is open (question 3).
- **The schema**: carried in the code of the agent that evaluates (the builder for the shared parts: the action
  and resource types that every agent has), so an author cannot change what a type is.

### The signal

On accepting an update the credentials agent records it in its ledger with a **global sequence number** (the ledger
already orders admin actions this way) and publishes it on a topic, `polar-authority-updates`:

```
AuthorityUpdate { kind: "policy" | "permissions", sequence, version, document: <the signed document, untouched> }
```

State-based, not a delta: the whole new policy or the whole new principal data, so a missed event is repaired by
the next, and a restarted agent needs only the latest. Consumers, in every builder agent:

1. check it came from the credentials agent (the broker says who published it);
2. check the document itself as for any signed document: the chain to the pinned CA, and that the signer holds
   the capability for its kind (`c:update-policy`, or the one for permissions). That root is the certificate
   capability, which needs no policy to check, so there is no circle;
3. apply it to the local `PolicyActor` or `PermissionsActor` if its version is higher;
4. raise `AuthorityChanged { kind, version }` on an output port of the infrastructure supervisor, as
   `ConfigChanged` is raised today, so any domain actor that holds something derived from authority (a session, a
   cached decision) can drop it at once.

**Catch-up.** Cassini does not yet acknowledge deliveries, so an update can be missed. Every agent asks the
credentials agent for the current policy and principal data when it registers (as it asks the polar scheduler for
its configuration), and the credentials agent also republishes the current versions on an interval. A consumer that
sees a version higher than its own it did not receive asks.

**Ledger-originated changes.** A revocation or a grant made by a signed admin action changes principal data too
(a revoked certificate stops being a signer). The same signal carries it, so a demotion is immediate at the
leaves. How a revocation reaches the signer-verification seam in `signed.rs` (today `NoRevocation`) is the same
mechanism: the leaf holds the revoked serials as part of the principal data.

## Evaluating at a leaf

A Cedar-backed `Authority` plugs into the trait the verifier already has. For a document or command the request is:

- principal: the signer, `Identity::"<id>"`, whose attributes and groups `PermissionsActor` resolves from the
  certificate plus the supplemental data;
- action: `update-configuration`, `update-runtime`, `run-command`;
- resource: `Section::"<agent>/<section>"`, `Variable::"<agent>/<NAME>"` or `Command::"<agent>/<name>"`, built
  from the agent's own declarations;
- context: the time, and what a rule needs.

**The certificate capability stays the floor.** It is checked first, offline, with no policy loaded (the bootstrap
case: the first policy and the first permissions are signed by someone holding the capability, and need no policy
to be believed). When a policy is loaded, Cedar must **also** permit: policy tightens, and the capability is what
admits a signer to be considered at all. Whether a policy may also *admit* a signer who lacks the capability is
open (question 2).

## What the builder gains

- `PermissionsActor` and `PolicyActor` in every builder agent, started by the infrastructure supervisor beside the
  configuration and command actors, with the shared schema.
- A built-in subscription to `polar-authority-updates` and the request for the current state on registration.
- `AuthorityChanged` on the infrastructure supervisor's ports.
- The Cedar `Authority` for configuration, runtime documents and commands, behind the existing trait.
- Declarations feed the resources: an agent's sections, variables and commands are the resource types.

## What the credentials agent gains

- It accepts the `policy` and `permissions` documents (as runtime sections of its own, `credentials-policy.md`),
  validates them, records them in the ledger with a sequence, and raises the signal.
- It serves the current state on request and republishes it on an interval.
- Its own `PolicyActor` and `PermissionsActor` are the same actors the leaves run.

## Build order, tests first

1. The shared evaluation as a pure function: given a principal's data, a policy and a request, the decision, with
   the attack list (a document cannot set `permissions`, a forbid beats a permit, the capability floor with and
   without a policy).
2. `PermissionsActor` takes group membership, and `PolicyActor` takes the shared schema and rejects what does not
   validate. The invariant that nothing but `PermissionsActor` supplies principal data is a test.
3. The `AuthorityUpdate` message, its verification at a leaf, versioning, and `AuthorityChanged`.
4. The builder hosts the actors and the Cedar `Authority`; the verifiers use it.
5. The credentials agent accepts, records and raises; serves and republishes.
6. The seed policy and permissions in the schedules repository, and a cluster test: a demotion reaches a leaf and
   is refused there within a second.

## Questions (open)

1. **Trust in what is pushed.** Is the broker's statement of who published (the credentials agent's identity) plus
   the author's signature on the untouched document enough? Or should the credentials agent sign what it
   publishes with a key the leaves pin (which key: it has its escrow keypair and its mTLS certificate)? Ledger-originated
   changes (a revocation) have no author document to forward, so they need one or the other.
2. **Does policy only tighten, or may it admit?** The proposal above: the capability is the floor and Cedar tightens.
   The alternative: once a policy is loaded, Cedar is the whole decision and a signer without the capability may be
   admitted by it.
3. **Which capability authors principal data?** `c:grant` exists ("grant permissions to an existing identity"),
   and putting someone in a group is arguably a grant. A new capability costs a redeployment of the consumers; it
   would let group editing and permission granting be separate authorities.
4. **Catch-up cadence.** Republish on an interval so that a missed update is repaired: what interval is acceptable
   for the worst case of a missed demotion, until delivery is acknowledged?
