# Signed configuration

Status, 2026-10-11: design, agreed in outline with David; nothing here is built. It is the
"signed configuration" item in `docs/architecture/configuration.md` section 6, and it sits in front of
`ConfigurationHandle::apply` (`polar-agent-builder/src/configuration.rs`).

## Purpose

The person who writes a schedule or a configuration override signs it, and whatever applies it
verifies the signature first. A writable topic is not authorization, and neither is a repository a
push reached. The signature is what ties a change to a person, and it travels with the change to
the place that acts on it, so no relay in between has to be trusted.

## Decision (David, 2026-10-11): both forms

1. **A signed update** is the trust root. The signature covers the update itself and travels with
   it: in the repository next to the schedule, then through the observer, the processor and the
   topic, to the ConfigurationActor, which verifies it.
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
- **The key.** ECDSA P-256, key usage `digitalSignature`, extended key usage `clientAuth` only,
  valid one year, issued by the Polar CA. There is no revocation anywhere yet.

## The signed update

**Payload.** What the ConfigurationActor would apply, and nothing else: the schedule id, the target
(agent id or agent type), the version, and the changes (variable name and value). It is encoded
canonically: UTF-8 JSON, object keys sorted, no insignificant whitespace, preceded by a fixed
domain-separation line (`polar-config-update-v1`) so a signature made for this purpose cannot be
replayed as anything else.

**Envelope.** The payload, an ECDSA P-256 SHA-256 signature over the canonical bytes, and the
signer's certificate chain (PEM). Stored as `<schedule>.sig` beside `<schedule>.dhall`.

**Why the evaluated form.** A schedule is a Dhall file, but the observer evaluates it and what travels
is JSON. A signature over the source could not be checked by a receiver that never sees the source.
So `polar-sign` evaluates the file with the same engine the observer uses (`serde_dhall`) and signs
the canonical result. The observer evaluates again and attaches the envelope; if the two evaluations
differ, verification fails, which is the safe outcome. Imports are the way they could differ, so see
open question 1.

**What the receiver checks, in this order, refusing on the first failure and recording it:**

1. The envelope parses and its payload is the canonical form (re-encoding it gives the same bytes).
2. The certificate chain verifies to the Polar CA the receiver already pins, and the certificate is
   valid now, with `digitalSignature` key usage and an EC P-256 key.
3. The signature verifies over the payload.
4. The signer is authorised to make this change (below).
5. The version is higher than the last applied: this is the ConfigurationActor's existing check.
6. Only then is `apply` called. The ConfigurationActor still refuses a Static variable, a bad value
   and a result the sections' relations reject, whoever signed.

Until a receiver has what it needs to verify (the CA, a policy) it **fails closed**: it refuses.

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
- **`polar-sign` (binary).** Git signing mode; `sign` and `verify` for updates (usable as CI on the
  schedules repository); `setup`. Good `--help`, and an optional configuration file for the key and
  certificate paths, in the platform's standard configuration directory, so no `just` is needed.
- **`polar-admin-grant` stays as it is.** Binaries are named `polar-<name>` so an umbrella `polar`
  command can dispatch to them later, as `git` and `cargo` do, if the set of tools keeps growing.
- **Platforms.** Linux on AMD64 and ARM64 (static) and Windows on AMD64 build with nix cross
  targets. macOS needs a darwin builder or a macOS CI runner and is not believed to cross-build from
  Linux; Windows on ARM64 is uncertain. Each target is verified before it is promised.

## Open questions

1. **Imports in signed Dhall.** Forbid them, or require frozen (hash-pinned) imports? Either keeps
   the two evaluations identical.
2. **Revocation.** A signer removed tomorrow still has valid old signatures, and their certificate is
   valid for a year. Options: a revocation list the receiver consults, or a maximum age for a signed
   update. Not needed for the first version; needed before this is relied on.
3. **Git server enforcement.** Should the git server refuse a push whose commits are not signed by the
   pusher? Later, and independent of the above.
4. **Mapping a `ScheduleNode`'s `config` to variable names.** Shared with the scheduler integration
   in the ConfigurationActor item.
5. **Wire types.** `GitScheduleChange` and `ScheduleNotification` must carry the envelope; the
   processor passes it through untouched.

## Build order, tests first

1. `polar-signing`: canonical encoding and its stability (key order, whitespace, unicode, numbers).
2. Sign and verify: a valid signature; an altered payload; a wrong key; a certificate from another CA;
   an expired certificate; a certificate without `digitalSignature`; the wrong domain prefix.
3. Authority: a signer with `c:update-policy` is accepted, one without is refused.
4. The ConfigurationActor in front of `apply`: unsigned, wrongly signed, unauthorized signer and an
   old version are all refused and leave the value as it was.
5. `polar-sign` update signing and `setup`; then the git program mode against the installed git.
6. The wire types and the observer attaching the envelope; then the nix targets.
