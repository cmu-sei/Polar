# Credentials agent policy

Status, 2026-10-12: design, agreed in conversation with David; nothing in it is built except the checks noted
as built. It says how the credentials agent decides who may have a secret, where that decision's inputs
(rules, groups, secret attributes) are stored, and how they reach the agent. It is the first user of the
`runtime` document type in `git-controller.md`.

David's direction: certificates were always a coarse mechanism; signed policy updates are the opportunity to do
much better, and the expressiveness gained from Cedar's more advanced features justifies the added complexity.

## What exists, and what does not (read from the code, 2026-10-12)

- `polar-policy` wraps Cedar. Its `PolicyActor` holds a `PolicySet` parsed from a source string
  (`UpdatePolicy { policy_source }`). It never links templates and validates against no schema.
- For each request it builds **one entity**: the requester as `Identity::"<id>"` with a single attribute,
  `permissions` (resolved from the certificate by `PermissionsActor`), and **no parents**. The resource is only
  an ID string, not an entity: it has no attributes and no parents. The context is empty.
- **Nothing loads a policy in production.** `UpdatePolicy` is called only from tests. The admin action
  `PolicyUpdate` exists in the wire types and is refused as `NotYetSupported`. With no policy set the engine
  says "no policy set has been loaded yet".
- `RetrieveSecret` returned any stored secret to any authenticated requester that knew its name, encrypted to the
  requester's mTLS key. Fixed 2026-10-12: it now asks policy (`Action::"retrieve"`, resource
  `Secret::"<name>"`), fails closed, and encrypts to the requester's agent escrow key. `DecryptOnBehalfOf`
  already asked policy (`Action::"decrypt"`), still encrypts to the mTLS key, and is not changed here.
- Consequence today: every retrieval and every decrypt fails closed, because no policy is ever loaded.

## The design

### The `policy` section

The credentials agent declares one runtime section, `policy` (`git-controller.md`, "The `runtime` document
type"). It is signed with `c:update-policy`, the capability that already exists for this, and delivered like any
runtime document: the same topic, the same verification, the same outcome events, and replay to an agent that has
just started. The latest version replaces the one before, whole.

```
payload = {
  type: "runtime", id, target: { agent: "credentials-agent-consumer" },
  section: "policy", version,
  body: {
    policies: "<Cedar source text>",
    entities: [ ... groups, memberships, and attributes on secrets ... ]
  }
}
```

The **schema is not in the document**. The credentials agent carries it in its own code, so an author cannot
change what a type is. A document is **rejected whole, and the previous policy stays in force**, unless:

1. the Cedar source parses;
2. every policy validates against the agent's schema (a misspelt attribute or an action the schema does not
   know is an error at signing time and at receipt, not a mystery denial later);
3. the `entities` parse and validate against the schema, and use no reserved attribute (below).

The outcome is reported like every runtime document (applied, or refused with the reason and the signer).

### Where groups are stored

Cedar has no store of its own: a group is an entity and membership is a `parents` entry on the member's entity,
given to every request. **Groups live in the `entities` list of the signed policy document**, beside the rules.
They have the same authority as the rules, the same versions and the same review. A restarted agent gets them
back by replay from the schedules repository. The graph is never read for them.

```
"entities": [
  { "uid": { "type": "Group", "id": "credential-managers" }, "attrs": {}, "parents": [] },
  { "uid": { "type": "Identity", "id": "alice" }, "attrs": {}, "parents": [ { "type": "Group", "id": "credential-managers" } ] },
  { "uid": { "type": "Secret", "id": "github-sei" }, "attrs": { "kind": "git-token" }, "parents": [] }
]
```

How the agent uses them: it takes the declared entities as its entity set, then for each request **adds or
completes** the two entities that carry facts it knows better than any document:

- The **principal** `Identity::"<id>"`: its `permissions` attribute from the certificate (as today), and its
  parents from the declared entity of the same id, if there is one. A document cannot override `permissions`.
- The **resource** `Secret::"<name>"`: its `owner` from the ledger (`stored_by`, the identity that escrowed it).
  A document can add attributes to a secret (`kind`, `frozen`) and cannot set or change `owner`, which is
  reserved and rejected.

Identities in a document are named by what the broker names them. For an agent that is its workload name
(`git-repo-observer`). For a person it is the account identity, which is a UUID; **open question**: let the
document use the username and have the credentials agent resolve it to the identity from its account records
(it holds the provisioned accounts), so that an author writes `alice`, not a UUID. Not verified that the records
carry what is needed.

### What a request looks like to Cedar

| | Today | Under this design |
|---|---|---|
| Principal | `Identity::"id"`, attr `permissions`, no parents | the same, plus parents from the declared groups |
| Action | `Action::"retrieve"` or `"decrypt"` | the same; others added as needed (`store`, `grant`) |
| Resource | an ID string, no entity | `Secret::"name"` with `owner` (ledger) and declared attributes |
| Context | empty | `{ now_secs }`, and later what a rule needs (the requester's channel, an environment) |
| Schema | none | the agent's own, validating policies and entities on receipt |

### Expressiveness this buys, as it applies to secrets

Written out in the conversation; the ones this design supports first:

```cedar
// An agent may retrieve what it escrowed itself.
permit (principal, action == Action::"retrieve", resource)
when { resource.owner == principal };

// A group may retrieve one kind of secret.
permit (principal in Group::"credential-managers", action == Action::"retrieve", resource)
when { resource has kind && resource.kind == "git-token" };

// Incident freeze: overrides every permit.
forbid (principal, action, resource)
when { resource has frozen && resource.frozen };

// Time-limited access for a group.
permit (principal in Group::"contractors", action == Action::"retrieve", resource)
when { context.now_secs < 1767225600 };
```

Cedar denies by default, and any matching `forbid` beats any `permit`. A loaded policy with no matching `permit`
is a denial. **No policy loaded at all is a failure** (so a client knows to retry), and says nothing about
which secrets exist.

What Cedar does not give us, and stays in code: rate limits (it is stateless), wildcard host bindings (`like`
takes only a literal pattern, so "a credential only goes to hosts matching its own stored pattern" cannot be a
rule over a stored attribute), and network checks.

### The first policy

With no policy, nothing is retrievable. The first document is therefore the smallest useful one, in the schedules
repository next to the credentials agent's configuration item:

```cedar
permit (principal, action == Action::"retrieve", resource) when { resource.owner == principal };
forbid (principal, action, resource) when { resource has frozen && resource.frozen };
```

With it, the observer's flow works with no per-secret authoring: it receives a credential in a document, uses it,
escrows it (`StoreSecret`, so `owner` is the observer), and retrieves it at init (`RetrieveSecret`).

### Trust and where it is enforced

The **credentials agent evaluates this policy**, for the secrets it holds. A separate question, left for later, is
policy for other receivers (who may sign which runtime section for which agent, who may run which command): they
would evaluate on their own side, so the policy set and the groups would have to be delivered to each of them
as signed documents of their own. The credentials agent's design does not depend on that. It does not replace
the capability check; it sits beside it: a signer still needs `c:update-policy` for a `policy` document, and
that capability is the coarse gate. Cedar is the fine one for secrets.

## Explaining a decision

Cedar reports which policies decided a request. The credentials agent should record, for every decision, the
requester, the action, the secret's **name**, the outcome and the ids of the deciding policies, in its ledger
or log. Never the secret or anything derived from it. A refusal then says why.

## What changes in the code, in order, tests first

1. **The request as Cedar sees it, as a pure function** (in `polar-policy` or beside it): given the declared
   entities, the certificate permissions, the ledger owner and the context, produce the entity set and the
   request. Tests: the owner rule allows the owner and denies another identity; a group member is permitted by a
   group rule and a non-member is not; a `forbid` beats a `permit`; a document cannot set `owner` or
   `permissions`; a secret with no declared entity still has its owner.
2. **The schema and the validation of a document**: an unknown attribute, a wrong type, an unknown action, and
   a reserved attribute are each refused and leave the previous policy in force. Tests first.
3. **`polar-policy` takes the entities and context**, replacing the principal-only entity set, and the
   credentials agent builds the request for `RetrieveSecret` and `DecryptOnBehalfOf` through (1). The existing
   tests that permit by identity keep passing.
4. **The `policy` runtime section** on the credentials agent, once the `runtime` document type exists
   (`git-controller.md`, step 2), including replay. The unimplemented `PolicyUpdate` admin action is retired,
   or left refusing, since the section replaces it.
5. **The decision record** (the ledger event or log line above).
6. **The first policy** in the schedules repository, and a cluster test: an agent stores a secret, retrieves it,
   and is denied another agent's.

## Open questions

- Usernames or identities in `entities` (above).
- Which attributes a secret carries beyond `owner`. A secret is stored with a name and ciphertext only. Declared
  attributes in the document annotate it by name; whether a secret should instead carry labels from the moment
  it is stored is for later.
- `DecryptOnBehalfOf`'s resource is a caller-supplied Cedar reference, not a name the agent builds: it should
  take a name and build the entity the same way, with the same name validation.
- Whether the same `policy` document should also carry the rules other receivers evaluate, or each receiver
  gets its own section (see "Trust and where it is enforced").
