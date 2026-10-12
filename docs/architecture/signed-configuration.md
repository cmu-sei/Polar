# Signed configuration

Status, 2026-10-11: design, agreed with David (including the single-file form below). Built so far: the `polar-signing` library (`src/agents/polar-signing`): the canonical encoding, the two-field document layout, sign, verify and the version-one authority rule, with tests. The ConfigurationActor verifies a signed document before it applies anything (`polar-agent-builder`: `ConfigurationHandle::submit`, `signed.rs`). Delivery is built too: the scheduler processor recognises a signed document, writes a `Configuration` node to the graph, and publishes a `ConfigurationNotification` on `agent.<name>.configuration`, and every builder agent subscribes to its own topic and hands what arrives to `submit`. Not built: `polar-sign`, the Dhall rendering, the outcome events, encryption of secrets, the git integration and everything else below. It is the
"signed configuration" item in `docs/architecture/configuration.md` section 6, and it sits in front of
`ConfigurationHandle::apply` (`polar-agent-builder/src/configuration.rs`).

## Purpose

The person who writes a schedule or a configuration override signs it, and whatever applies it
verifies the signature first. A writable topic is not authorization, and neither is a repository a
push reached. The signature is what ties a change to a person, and it travels with the change to
the place that acts on it, so no relay in between has to be trusted.

## Decision (David, 2026-10-11): both forms

1. **A signed document** is the trust root. The signature covers the whole configuration item and
   travels inside it: in the repository as one file, then through the observer, the processor and
   the topic, to the ConfigurationActor, which verifies it. There are no signature files.
2. **Signed git commits** are made with the same key, so the history shows who changed what and the
   repository contents carry signatures all the way to where they are needed. They are for audit
   and context, not the trust root: they are checked where the repository is read, and do not
   survive the hop over a topic.

## What a certificate already tells us

Read from `cert-issuer` (`server/src/ca.rs`, `common/src/capability.rs`) and a real user certificate:

- **Who.** The Subject is `UID=<username>, mail=<email>`. Both are written by cert-issuer from the
  account record the credentials agent matched against its ledger. A certificate request that
  claims its own `uid` or `mail` is ignored. Given name, surname, title and telephone number are
  optional and come from the request. The first DNS SAN is a per-cluster UUID, the machine-facing
  identity the git server and Cassini use.
- **What it may do.** Permissions (`p:`) and capabilities (`c:`) ride as URI SANs. Capabilities are
  a **closed enum** (`ProvisionAccount`, `Grant`, `Revoke`, `UpdatePolicy`); `c:update-policy` is
  documented as "submit a policy update". The enum cannot carry names, so it cannot say "this agent"
  or "this variable".
- **The key.** Key usage `digitalSignature`, extended key usage `clientAuth` only, valid one year, issued by
  the Polar CA. A user's certificate seen so far has an ECDSA P-256 key; the bootstrap admin's has an
  Ed25519 key. The credentials agent's `admin_signing` already signs administrative actions with either, so
  this system accepts both. There is no revocation anywhere yet.

## The signed document (one file per configuration item)

**Layout.** One Dhall file per item, a record with exactly two fields (the signature block has exactly
`algorithm`, `value` (base64) and `certificate_chain` (a list of PEM strings, the signer's first), and a
document with any other field is refused): `payload`, which is all of the
item (the schedule id, the target agent or agent type, the version, the changes, and the descriptive
metadata), and `signature`, which holds only the signature value and the signer's certificate chain.
A signature cannot cover its own bytes, so `signature` is the one thing excluded from what is signed;
it carries no behaviour. Nothing that changes what happens sits outside the signed part, so a pusher
with write access cannot redirect a signed item to another agent or replay it under another version
without invalidating it. Transport encryption (mTLS to the git server, mTLS to Cassini) protects each
hop; the signature is what says who authored the content and that it is unchanged, at every hop and
on a remote we do not host.

**Authoring and rendering.** The user writes the item in Dhall, using the agent's template and any
imports they like. `polar-sign` then evaluates it to normal form, which has no imports, checks it,
signs it, and writes the final single file. What is committed is that rendered file. The observer
evaluates a file with nothing left to resolve, so the signer's and the observer's evaluations cannot
differ, and the scheduler sends only a document that has already been verified once. A committed file
that still contains an import is refused.

**What is signed.** The canonical encoding of `payload` after evaluation: UTF-8 JSON, object keys
sorted, no insignificant whitespace, preceded by the domain-separation line
`polar-config-document-v1` so a signature made for this purpose cannot be replayed as anything else.
The signature is Ed25519 over those bytes, or ECDSA P-256 over their SHA-256, whichever the signer's
certificate has (both are deterministic, so the same input gives the same signature). (Dhall defines a standard semantic hash that
would be the more natural thing to sign, but `serde_dhall` does not expose it, and using it would mean
depending on the fork's internals. Canonical JSON is ours to define and to test. Revisit if the library
exposes it.)

**Failing early.** Users should not find out later that an item was rejected. Two things serve this:

- *Typed templates.* A Dhall record type is generated for each agent from its schema export
  (`<agent> --scaffold`), listing only its `Dynamic` variables, so a wrong name or type fails in
  Dhall's own type checker before anything is signed.
- *`polar-sign check`.* Tests the rendered payload against the agent's declarations (the same
  parsing the ConfigurationActor uses), the version against the last known, and the signer's
  certificate (valid, `digitalSignature`, carries the capability), and reports every problem.

**What the receiver checks, in this order, refusing on the first failure and recording it:**

1. The document parses, has exactly the two fields, and the signature block has exactly its three.
   (A committed document that still contains a Dhall import is refused where the file is read, before
   it becomes JSON: not built.)
2. The signature block's chain verifies to the Polar CA the receiver already pins, and the signing
   certificate is valid now, with `digitalSignature` key usage and a key of a type this system signs with,
   the one the document claims. The signature verifies over the canonical encoding of `payload`.
3. Only now is `payload` read, since until the signature held it is not to be trusted. It must have the
   shape of an update (below), with no field the receiver does not know.
4. It is addressed to this agent, so a document signed for one agent cannot be applied by another.
5. The signer is authorised to make this change.
6. The signer is not revoked (a hook that always says no, until something revokes).
7. The version is higher than the last applied: the ConfigurationActor's existing check.
8. Only then is the update applied. The ConfigurationActor still refuses a Static variable, a bad value and
   a result the sections' relations reject, whoever signed.

**The payload of an update** is `{ type: "configuration", id, target: { agent }, version, changes: [ { name, value } ] }`, all
strings except the version, which is a whole number. Every signed payload carries a `type` (`configuration`, `admission` or
`command`; `scheduling-kinds.md`) and a receiver accepts only the kind it expects, checking it before it reads anything else of
the payload, so a command cannot be taken for a configuration whatever else it says. A document with no `type` is refused. `target.agent` is the receiving agent's own name (its
service name) for now; how a schedule's agent id and agent type map onto that, and where a `ScheduleNode`'s
`config` becomes `changes`, are the addressing questions still open.

**Built (2026-10-11).** `ConfigurationHandle::submit(document)` is the only way in; the unverified
`apply` is crate-private and used only by tests. The verifier takes its trust from the CA the agent already
pins for Cassini (`TLS_CA_CERT`), and an agent with no readable CA refuses every document (it fails
closed). The history records the signer (username or identity, and serial) of every verified update; a
document refused before its payload could be trusted is recorded with version zero and no signer, and no
value of any kind is ever recorded.

Until a receiver has what it needs to verify (the CA, a policy) it **fails closed**: it refuses.

## How it fits together

Three sequence diagrams, in `docs/architecture/sequences/general/` (rendered as `asset_regeneration.md` describes):

- `signed_configuration_authoring.mmd` (and `.png`): the author's side. A typed template from the agent's
  schema export, `check`, `sign`, the commit (signed through git's x509 support with the same key) and
  the push, ending with the git server announcing the change.
- `signed_configuration_delivery.mmd` (and `.png`): the runtime side. The announcement, the observer's pull and
  evaluation, the scheduler's storage and notification, and the ConfigurationActor in the target agent
  verifying the whole document for itself before it applies anything.

- `signed_configuration_secrets.mmd` (and `.png`): a secret travelling in a document: the author's tool
  gets the target agent's public escrow key and encrypts to it, the target decrypts through its
  CryptoActor, closes the loop with the credentials agent, and retrieves the secret later.

**The schema export** is what `<agent> --scaffold` prints (`polar-env`'s document module): every
declared variable with its type, whether it is required, whether it is secret, its default, its
`static` or `dynamic` mutability and its description, by section, plus what the agent requires and its
minimum shutdown budget. A schedule may set only the `dynamic` ones, so the generated template lists
only those. Today no variable in the workspace is declared dynamic outside tests, so a generated
template would be empty until agents declare some.

**Verification happens twice, on purpose.** The observer checks a document early, with the same rules,
and drops and logs an invalid one, so the scheduler only ever carries documents that were good when they
were read. The ConfigurationActor checks again, because it must not trust the relay. The two use the
same library, so they cannot disagree about the rules.

**What the author sees (built).** Every document an agent handles, applied or refused, produces a
`ConfigurationOutcome` event on `polar-configuration-outcome`: the agent, the id and version the document
claims, a status (`applied`, `unchanged`, `stale`, `invalid` or `unverified`), the names that changed or the
reasons it was not applied, who signed it (only if it verified), a time and a sequence number. Never a value.
The scheduler processor writes each as a `(:Schedule:ConfigurationOutcome)` node, one per event so the trail is
kept, linked `(:Schedule:Configuration)-[:HAD_OUTCOME]->` to the item it names, with the identity the broker
gives for whoever published it as `reported_by` (an outcome is only a claim by its sender, so the graph keeps
who sent it apart from who it says it is about). So an author can see in the graph that a pushed document was
refused and why. A tool that asks the graph for this on the author's behalf is not built.

**Not designed yet:** where an author's machine gets an agent's schema export (the agent's image can print it,
but an author usually has no image; candidates are publishing it with each release, or committing it to
the schedules repository from CI).

## Secrets in a document

A configuration item can carry credentials: the credentials agent's own item carries policy and
permissions and the usernames and passwords to escrow for a named agent, and other agents' items can
carry a new credential for that agent. A secret is never plaintext in the repository.

**Which fields.** The schema export marks each variable `secret`. The typed template shows them, the
tool encrypts them, `check` refuses a plaintext value for one, and the receiver refuses a secret field
whose value is not ciphertext. The signature covers the ciphertext, so the signed document can be stored
in the graph and passed over topics as it is.

**Whose key it is encrypted to.**

- *An item for the credentials agent itself* (policy, and secrets to escrow for a named agent): encrypted
  to the credentials agent's own escrow public key (`EscrowKeyRequest`), which it stores directly
  (`StoreSecret`).
- *An item for any other agent*: encrypted to that agent's own escrow public key
  (`AgentEscrowKeyRequest`). The agent decrypts it with its CryptoActor (`DecryptOwnMessage`; the private
  key stays in custody), uses it, then **closes the loop** by escrowing it with the credentials agent
  (`StoreSecret`) so it can be retrieved later (`RetrieveSecret`, re-encrypted to the agent's own
  certificate). Closing the loop is retried until the credentials agent acknowledges it, and so depends on
  the agent receiving the document again if it restarts first (the scheduler's reconciliation item).
- *Noted alternative for other agents:* encrypt to the credentials agent's key and let the agent fetch with
  `DecryptOnBehalfOf` (policy-checked), as kube-consumer's environment values already work. No re-escrow
  step, at the cost of the per-agent compartmentalisation above.

**Binding.** Each ciphertext is bound to its agent, field name and version as authenticated data, so it
cannot be moved into another field or document. The signature already pins its place in the signed
document; this pins it to its recipient.

**At the receiver.** Decryption comes after every verification step, through the CryptoHandle the
infrastructure already holds, and the plaintext is applied and never recorded: the ConfigurationActor's
history and reports already keep names and never values, and a secret variable is hidden in reports.

**The tool needs the recipient's public key.** The author's machine is not on Cassini, so it cannot ask for
an escrow key. The credentials agent's HTTP front gets a read-only endpoint, "the public escrow key of agent
X", authenticated by the author's user certificate and returning the public key only. Not built.

**A property to keep in mind.** Ciphertext committed to git stays in its history, in every clone, and in the
graph. It stays safe only while the private escrow key it was encrypted to stays private; a later
compromise of that key exposes every secret ever pushed to it, because nothing here gives forward secrecy.
So a secret in the repository is a delivery vehicle: change the credential's value after the agent has
stored it, and prefer having the git server refuse unsigned pushes so a plaintext secret cannot be slipped
in. The encryption scheme is the one the CryptoActor already uses (`EncryptForRecipient`); confirm its
properties when this is built.

## Authority

**Version one:** the signer's certificate must carry `c:update-policy`. That is coarse: it permits
signing any update, to any agent. It is the one existing capability meant for this, needs no new
issuance mechanism, and is checked by a small pure function with tests.

**Later:** "may this signer set this variable on this agent" is finer than the closed capability enum
can say, so it becomes a policy question for `polar-policy` (Cedar), asked through a trait so a
Cedar evaluator replaces or tightens the first check without touching the verifier, as with the git
server's authorizer. The policy source is itself configuration and must be trusted without this
mechanism (the bootstrap circle the git server design also avoids), so it comes after the first
version works.

## Git

**Not a hook.** Git has native X.509 signing. Setting `gpg.format=x509` and `gpg.x509.program` makes
git run that program to sign a commit (stdin to a detached signature) and to verify one; `smimesign`
works this way. So `polar-sign` implements the small `gpgsm`-style command line and status output git
expects. This describes git's interface from memory and must be checked against the installed git
before it is built.

**Contained to the policy repository.** `polar-sign setup`, run inside a clone, writes only that
clone's local `.git/config`: `gpg.format`, `gpg.x509.program`, `user.signingkey`, `commit.gpgsign`,
and `user.name` and `user.email` from the certificate's `uid` and `mail`. Nothing global changes and
the user's other repositories are untouched.

**The email.** The certificate's `mail` is authoritative, so nothing needs to be mapped or
configured. The verifier compares a commit's author email with the signer's certificate `mail`: a
warning at first, an error if configured so.

## Tool and scope

- **`polar-signing` (library).** Canonical encoding, sign, verify, chain and key-usage checks, and the
  authority trait. Used by the ConfigurationActor (verify) and the tools (sign).
- **`polar-sign` (binary).** Git signing mode; `sign`, `check` and `verify` for configuration documents
  (`check` and `verify` usable as CI on the schedules repository); `template` (generate an agent's
  typed template); `setup`. Good `--help`, and an optional configuration file for the key and
  certificate paths, in the platform's standard configuration directory, so no `just` is needed.
- **`polar-admin-grant` stays as it is.** Binaries are named `polar-<name>` so an umbrella `polar`
  command can dispatch to them later, as `git` and `cargo` do, if the set of tools keeps growing.
- **Platforms.** Linux on AMD64 and ARM64 (static) and Windows on AMD64 build with nix cross
  targets. macOS needs a darwin builder or a macOS CI runner and is not believed to cross-build from
  Linux; Windows on ARM64 is uncertain. Each target is verified before it is promised.

## Open questions

1. **Revocation (planned, not in the first version).** A signer removed tomorrow still has valid old
   signatures, and their certificate is valid for a year. The verifier consults a revocation source at
   step 5; the first version has none and says so. Options for the source: a list the credentials
   agent publishes, and a maximum age for a signed document.
2. **Imports (decided).** Imports are allowed when authoring and are resolved away by rendering; a
   committed document may contain none.
3. **Git server enforcement (decided, to build later).** A per-repository setting on the git server
   that refuses a push whose commits are not signed by the pusher: the signing certificate's identity
   must be the TLS peer's. On for `polar-schedules`, off by default, because a git server we do not
   run cannot enforce it. There the consumer's own verification of the embedded signature is what
   protects.
4. **Mapping a `ScheduleNode`'s `config` to variable names.** Shared with the scheduler integration
   in the ConfigurationActor item.
5. **Wire types.** `GitScheduleChange` and `ScheduleNotification` must carry the whole document
   (payload and signature), and the processor passes it through untouched.

## Build order, tests first

1. `polar-signing`: canonical encoding and its stability (key order, whitespace, unicode, numbers), and
   the document layout (exactly two fields, no import).
2. Sign and verify: a valid signature; an altered payload; a wrong key; a certificate from another CA;
   an expired certificate; a certificate without `digitalSignature`; the wrong domain prefix.
3. Authority: a signer with `c:update-policy` is accepted, one without is refused.
4. The ConfigurationActor in front of `apply`: unsigned, wrongly signed, unauthorized signer and an
   old version are all refused and leave the value as it was.
5. `polar-sign` render, check and sign, then `setup`; then the git program mode against the installed git.
6. The wire types, and the observer sending the verified document; then the nix targets.
