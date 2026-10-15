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

> **Decided 2026-10-12 (`authority-distribution.md`): groups are principal data and live with `PermissionsActor`, not in the policy document, which carries rules only. The `entities` list below is therefore withdrawn for groups; read this file for the credentials agent's own use, and that one for how policy and permissions are pushed to every agent.**
>
> **Was under review, 2026-10-12.** Reading `polar-permissions` and `polar-policy` after this was written: they
> deliberately keep *data about a principal* (what `PermissionsActor` resolves: the certificate's permissions
> plus supplemental permissions pushed by gitops) apart from *rules over that data* (`PolicyActor`), and the
> policy actor "derives a principal's own attributes exclusively from PermissionsActor ... there is no path, for
> any caller, to supply or influence a principal's own entity data". The `entities` list below puts group
> membership, which is data about a principal, into the policy document. Whether groups belong there, or in
> `PermissionsActor` as another signed input of their own, is open; see the questions at the end of this file.
> Nothing here is built.

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

Identities in a document are named the way their holders are known. An agent is its workload name
(`git-repo-observer`). A person is their **username**, and the credentials agent resolves it to the account
identity from its own records: the ledger's `AccountCertificateRecord` holds `username` and `account_id` (the
UUID that is the first DNS name in the user's certificate), usernames are unique among accounts that are not
revoked, and a revoked account re-provisioned under the same username gets the same `account_id` back. So an
author writes `alice`, never a UUID, and the name keeps meaning the same account. A username that no account has
is an error in the document (refused whole), not a silent no-match.

### What a request looks like to Cedar

| | Today | Under this design |
|---|---|---|
| Principal | `Identity::"id"`, attr `permissions`, no parents | the same, plus parents from the declared groups |
| Action | `Action::"retrieve"` or `"decrypt"` | the same; `store` added (decided 2026-10-14, not built); `grant` as needed |
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

## Seeding the cluster

**Built 2026-10-12** (`infra/seed.nu`, the `seed` step of `infra/up.nu`; run on the dev cluster, where the credentials agent applied and published the policy; a secret retrieved under it has not yet been observed). The signer is the first administrator, who holds `c:update-policy`; the git server must list that account as an administrator (`POLAR_GIT_SERVER_ADMINS`) before a push is accepted. A document whose path is already in the repository is left alone, so a re-run never sends version 1 again.

Some policy has to be in force from the first minute, and the schedules repository starts empty (the Polar git
server creates the bare repository; nothing puts content in it). The cluster setup (`infra/up.nu`) therefore
gets a step that **seeds `polar-schedules`** with the cluster defaults, signed:

- the credentials agent's baseline `policy` document (the two rules above, no groups);
- anything else a cluster needs from its first minute (later: the credentials agent's own configuration item).

The step needs a signer, and signing is the same as for any author: a user certificate holding the capability
for the document type. Two consequences:

- The first admin that `admin-provision` creates holds `c:provision-account`, `c:grant`,
  `c:update-configuration`, `c:run-command`, `c:update-policy`, `c:update-runtime` and `c:update-groups` (the
  recipe grants them now; an account provisioned before it did needs another provisioned, or a reissue). The
  seeding signer is
  the operator account, made after the stack is up, so the step runs after the admin steps and the git server
  and before anything that needs a secret.
- A cluster whose seed has not been pushed has no policy, and every retrieval fails closed, which is the right
  failure: the seed step is part of "the cluster is up", and `up.nu` waits for the credentials agent to report
  the policy applied before it says so.

The seed documents live in the repository (`infra/seeds/signed/`, beside the seed the git server commits unsigned) and are signed at
setup, not committed signed: a signature made once by a person would otherwise be baked into every cluster.

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

## Secrets carry labels from the moment they are stored

Decided 2026-10-12 (David). A secret is stored with a name and ciphertext only today. `StoreSecretRequest` gains
`labels` (a small map of text to text, bounded in count and size), recorded in the `SecretStored` ledger event
beside `stored_by`, and they become the secret's Cedar attributes under the reserved prefix: a label `kind` is
`resource.kind`. `owner` stays reserved and comes from `stored_by`. A label cannot be changed once stored (a
store under an existing name is refused today, and stays refused); the `entities` list may still *annotate* a
secret by name for what policy needs to change after the fact (`frozen`), but an annotation cannot replace a
stored label. This is what lets "any `git-token`" be a rule from the first day.

**The details (decided 2026-10-14, David; built with the store check).**

- `labels` is a map of text to text on `StoreSecretRequest`, recorded in the `SecretStored` event (a `serde`
  default, so entries written before it still read), and each label is an attribute of the `Secret` entity.
- At most 8 labels. A key is lowercase letters, digits and underscores, starting with a letter, 1 to 32
  characters, so it is a plain Cedar attribute name. A value is printable ASCII, 1 to 128 characters.
- `owner` (from the ledger), `namespace` (computed from the name) and `frozen` (the annotation) are reserved and
  refused as labels.
- A label cannot change once stored. The owner may replace the secret's value, but a replacement whose labels
  differ is refused (`LabelsChanged`); to change a label, store under a new name.
- The `CryptoActor` attaches one label, `kind`, equal to the binding's kind (`token` or `user-password`), so the
  example above is `resource.kind == "token"`. No vocabulary specific to git is invented: the binding's hosts
  say where a secret goes.

## Storing is governed by policy

Decided 2026-10-14 (David). `StoreSecret` asks policy before the ledger accepts a name: action `store`, resource
`Secret::"<name>"`, with the request's labels as the secret's attributes. Until now the first identity to store a
name owned it, so another identity could store `github-sei` before the observer did, and the observer's own
store would be refused as `NameTaken` and its credential left pending. Not built.

**Naming (decided 2026-10-14, David).** A secret is named `<identity>/<name>`, for example
`git-repo-observer/github-sei`, as the agent's own escrow keypair already is
(`agent_escrow_keypair_secret_name`, identity-scoped). The credentials agent builds the `Secret` entity from the
validated name and sets its `namespace`, as an `Identity`, from the prefix. Policy is asked first: action `store`, the signer as principal, resource `Secret::"<name>"` with that
`namespace`. **Cedar first, with a built-in floor (decided 2026-10-14, David):** if the policy does not speak to the
request, or none is loaded, an identity may store a name in its own namespace, `<identity>/…`, and also
`agent-escrow-keypair:<its own identity>`, the name its escrow keypair is stored under. The floor is needed
because every agent escrows its own keypair through this same path when it starts, on a fresh cluster before any
policy has been seeded, and the seed needs agents running; it is the same model as runtime authority
(`authority-distribution.md`: policy first, the certificate as fallback). The seeded policy therefore has no
`store` rule, and Cedar can still forbid, or permit more than the floor does. The keypair's name is unchanged, so
there is no migration. The agent computes `namespace` from the name; Cedar's `like` takes only a literal
pattern, so the prefix cannot be matched in the policy. Cedar's `like` takes
only a literal pattern, so the prefix is computed by the agent and not matched in the policy. A repository still
names its credential by the short name (`github-sei`), and the observer resolves it to
`<its own identity>/github-sei`. The identity is the one policy knows an agent by, its workload name.

## `DecryptOnBehalfOf` and the resource it names

The request carries the ciphertext itself and a `resource_id` the caller asserts. The agent does not resolve a
reference to something it holds: it takes the caller's ciphertext and the caller's word for what the policy
should treat it as. Nothing binds the two: `EscrowedSecret` has no authenticated data, so a requester holding
the ciphertext of a secret it may not have can attach the `resource_id` of one it may. This is the same binding
that `signed-configuration.md` already requires of secret fields (the ciphertext authenticated to its place), and
it is not done here. Two ways to close it, to choose when `DecryptOnBehalfOf` is next touched: bind the
`resource_id` into the ciphertext as authenticated data when it is encrypted, and verify it on decrypt; or
make the request name a secret held by the agent, as `RetrieveSecret` does, so the reference is resolved by the
agent and not asserted by the caller. Either way the entity is built by the agent, from a validated name.

## Open questions

- Whether the same `policy` document should also carry the rules other receivers evaluate (who may sign which
  runtime section, who may run which command), or each such receiver gets a section of its own. See "What is
  still open: policy for other receivers" in `git-controller.md`.

## Questions raised by reading the existing design (2026-10-12), unanswered

1. **Where do groups live?** In the policy document (as written above), or as supplemental data in
   `PermissionsActor`, delivered by a signed section of their own? The second keeps the existing rule that rules
   and principal data are separate, and lets "who may put people in groups" be a different authority from
   "who may write rules". The first keeps rules and groups in one atomic version.
2. **Where does data about a resource live?** `owner` comes from the ledger. What of `frozen`, and any attribute
   that changes after a secret is stored: a ledger event, or an annotation in a signed document?
3. **Where does the evaluator live for receivers other than the credentials agent?** See
   `git-controller.md`, "What is still open: policy for other receivers".
4. **Is the repository the only route for policy?** The admin action `PolicyUpdate` (signed, strictly ordered
   by a global sequence, today refused as not yet supported) is the other route the earlier design named. The
   earlier design also says certificate permissions act as automatic `permit`s with overrides on top, and that
   policies name users by username.
