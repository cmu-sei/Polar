# Polar git server

Status, 2026-10-10: built and tested (unit tests, and end-to-end tests that drive the real `git`
CLI over generated mTLS certificates); not yet run on a cluster. Built: transport and identity, the
read and write paths, push authorization, the `RepoChanged` topic, seeding, runtime repository
creation, and the push log. Not built: the Cedar authorizer, `polar-sign` and signed-commit
verification. Crates: `src/agents/polar-git-server/{common,server}`.

## Purpose

`polar-git-server` hosts the git repositories that carry Polar's configuration: the schedules
repo, the per-agent configuration repos, and whatever else the deployment declares. It replaces
the development-only nginx + fcgiwrap container in `src/agents/git-server`.

It is a `PolarAgent` on the builder (ractor actors under the InfrastructureSupervisor) and a
catalog workload. It is core infrastructure, deployed by the `infra/` stack. It is optional: a
deployment may point the scheduler at any other git remote, at the cost of the managed features
listed under "Degraded mode".

## Transport and identity

- Git smart HTTP over mTLS. The user's `git` CLI is configured with `http.sslCert`,
  `http.sslKey` and `http.sslCAInfo` (their Polar user certificate and the Polar CA). No
  credential helper, no passwords, no tokens.
- The server never terminates TLS on behalf of something else. Client certificates are required on
  every connection (no `allow_unauthenticated`). Peer identity is taken from the verified
  handshake, never from a header. The pattern is Cassini's `listener.rs`
  (`extract_peer_identity`, `WebPkiClientVerifier`), which cert-issuer's `tls.rs` mirrors.
- Workload certificates (the scheduler observer, later the git agent) authenticate the same way,
  so reading the schedules repo needs no stored credential.
- Development: `kubectl port-forward` to the service, plus the matching `git config` lines.

## Git execution

Rust has no server-side `receive-pack` (gitoxide is client-side), so each request spawns
`git upload-pack --stateless-rpc` or `git receive-pack --stateless-rpc` against a bare repository,
under a supervised request actor with a timeout and a body size limit.

Push authorization happens before git runs. A push body begins with pkt-line ref-update commands,
ahead of the pack. The server parses those, authorizes each ref update for the peer identity, and
only then spawns `receive-pack`. No hook scripts, so nothing in git has to call back into actors.

## Authorization

- Default policy is static and lives in the server's configuration: per repo, the identities (by
  permission SAN or uid) allowed to read and to write. By default the scheduler observer is the
  only agent that may read, and admins write. Who the admins are is a fact about the deployment, so `POLAR_GIT_SERVER_ADMINS` names them apart from the repository declaration (and in addition to the `admins` in it); in development the targets name the accounts `just admin-claim` made, by the DNS name in their certificates (their ledger identity, which changes when the cluster is reset).
- The decision goes through one function behind a trait, so a Cedar evaluator (`polar-policy`)
  can replace or override it. Cedar-defined groups (for example "config policy authors") then
  extend who may push, with no change to the server.
- The policy repo is not hosted on the server it governs (bootstrap circle). Static first.
- Granularity is the repository. Things that need different authors are different repositories.
  Path-level rules would require quarantining a push and diffing trees; deferred, not designed.
  Branch rules are not used; each repo has `main`.

## Repositories

Two sources, one store.

1. **Declared at deploy time.** The git server's catalog entry lists bootstrap repositories
   (name, access list, optional seed tree taken from `infra/`). Render also derives a
   configuration repository for any agent entry that takes `config: "document"` once that
   convention is decided. Why the catalog: it is the one thing the check/render/apply pipeline
   reads, so a repository declared there is provisioned by the same apply that deploys the agent.
2. **Created at runtime.** `PUT /admin/repos/<name>` with an optional JSON body
   `{"readers": [..], "writers": [..]}`, allowed only to an administrator (identified by its client
   certificate like every caller). Idempotent: 201 when created, 200 when it already existed, and
   the access list is set to the body's either way. The deployment's declaration always wins: a
   repository the catalog declares answers 409 and keeps its list. The access lists of runtime
   repositories are kept in `runtime-acl.json` beside the repositories. The server's list of repos
   is its own state (a PVC of bare repositories); the catalog is the desired subset, not the whole
   list.

Seeding happens once, only when the repository is empty, so redeploying never overwrites later
edits. A declared repository's `seed` names a directory under the seed root; the trees are in
`infra/seeds/<repo>` and the Nix build puts them in the image (the server binary carries their
store path as its default seed root, which is also what makes the image include them).

## Notifications

After a push is accepted, the server publishes a `RepoChanged { repo, ref, old, new, pusher }`
message on a Cassini topic. Topics are not declared in the catalog; each agent keeps its topic
names as constants in its common crate (as credentials-agent-common does), and so will this one
(`polar-git-server-common`). The scheduler observer subscribes, so with the managed server it
need not poll; against another remote it keeps polling.

## Authorship, and degraded mode

The server's own record of pushes says who pushed what and when: `push-log.jsonl` beside the
repositories, one JSON line per accepted ref update (time, repo, ref, old, new, pusher), appended and
synced. It is append-only by construction but not signed or chained, so it is a record, not proof;
a signed log in the style of the credentials ledger is the later step. It is not the only proof,
because it does not exist when the repo is hosted elsewhere.

Authorship travels in the data: commits are signed with the author's Polar certificate key
(`gpg.format=x509` with a signing program that Polar ships, `polar-sign`), and the consumer
verifies the signature against the Polar CA chain and the certificate's permissions, whichever
remote the commits came from. Degraded mode (own remote) therefore keeps verified configuration,
loses push-time authorization, the push log and `RepoChanged` notifications, and needs `polar-sign`
on the author's machine but no other client.

## Build order and tests

1. This document and the work-file decision record.
2. Crate on the builder: TLS listener, identity extraction, request actor.
3. Repo store and one-time seeding.
4. Read path (`upload-pack`).
5. Write path (`receive-pack`, ref-command parsing, authorization).
6. `RepoChanged` topic.
7. Push log.

Tests run real `git` against the server (clone with a valid cert; unknown or unauthorized cert
rejected with no anonymous fallback; authorized push; rejected push to a repo the identity cannot
write; seed once and not again; idempotent `CreateRepo`; `RepoChanged` only after success).
