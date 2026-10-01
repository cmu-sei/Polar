# Remaining work

The single source of truth for what is left. Update it as items finish; do not rely on conversation memory.
Last updated: 2026-10-01 (evening).

## Done (2026-09-29 to 2026-10-01)

- [x] Observer `/admin-action` relay; admin CLI over HTTP (`--observer-url` explicit, no default)
- [x] `claim` subcommand (two-phase, ECDSA P-256 claim keys)
- [x] Configurable account certificate lifetime (`user_lifetime`, dhall `userLifetimeSecs`, 1 year)
- [x] Path A -> B achieved: bootstrap -> provisioned admin -> claimed cert -> browser -> Neo4j over mTLS -> password change
- [x] cert-issuer logs no longer contain bearer tokens or CSRs (`skip_all`; verified on a fresh pod)
- [x] Neo4j client-cert truststore: Polar CA only (fresh PKCS12 keystore in the `neo4j-cacerts` init container)
- [x] Justfile: `admin-bootstrap`, `admin-provision`, `admin-claim`, `user-cert-install`; startup comment and dev-shell banner updated
- [x] Static dev PKI (`tlsCerts`, `just tls`) removed; docs updated (design doc, README, DOCUMENTATION.md)
- [x] `infra/render.nu` renders only; `infra/up.nu` is the target-aware orchestrator (plan printed up front, every step resumable, pauses only for the browser and the Neo4j password)
- [x] `up.nu` re-applies Jobs correctly (delete-before-apply matched the single-quoted `'kind': 'Job'` the render emits)
- [x] Image set derived from the render: `infra/images.nuon` (name, tag, source) + `infra/images.nu`; `up.nu --dev` builds and pushes only what the target references; tag mismatches and unknown images fail before any build
- [x] Justfile: 17 per-image and bulk build/load/mirror recipes removed; `just image <names>` and `just images-all` replace them

## Next: orchestration and Justfile

- [ ] Real fresh-cluster `up.nu` run end to end (provision + claim path has only been exercised on the resume path); confirm wait selectors (`job-name=...`), `polar-neo4j` statefulset, `pgrep -f svc/<name>`; record the user's `nu --version`
- [ ] `images.nuon`: add rows for git-observer/processor/scheduler, openapi-observer/processor, oci-resolver, scheduler-observer/processor, build-processor (attrs are in the `git-agents`, `openapi`, `scheduler`, `build-agents` recipes); then delete those four recipes
- [ ] Justfile: the `Agent containers` recipe (hardcoded `nix build ... -o result-*` list) duplicates the image table; replace with `nu infra/images.nu push --all` once the table is complete
- [ ] Justfile: remove stale dev-PKI recipes (`setup-cert-issuer`, `issue-dev-certs`, `run-jwks`, `run-cert-issuer`) and their comment block; fix leftover references (startup comment `just cluster-build-all && ...`, comment mentioning `just cluster-load-*`, orphaned "Pull and push the local-path provisioner" comment)
- [ ] Justfile: wrapper to provision further users signed with an existing admin cert (`admin-provision` is bootstrap-only)
- [ ] Justfile: Neo4j password helper; cluster status/health recipe; check `cluster-ctx`/`cluster-reset` against what `up.nu` expects
- [ ] Chromium variant of `user-cert-install` (`~/.pki/nssdb`; someone else tests it)
- [ ] `images.nu`: confirm `result/` is gitignored; check nothing else reads the old `result-*` symlinks (docs, other recipes)
- [ ] Optional later: render injects image tags from `images.nuon` into the Dhall (single source of truth); content-hash tags would end the `IfNotPresent` + `:latest` hazard

## Credentials-agent and identity

- [ ] Admin `reissue` action: revoke old cert, return account to pending with the same `account_id`, holder reruns `claim` (provisioning an existing username is refused today: `AccountAlreadyExists`)
- [ ] Expiry check on admin actions (the consumer appears not to check the author cert's validity period; verify)
- [ ] Investigate why the consumer pod died shortly after the first provision in the previous cluster (never got its logs)
- [ ] Remove the old blind path: GRANTS / REVOCATIONS / ACCOUNT_PROVISIONING subscriptions and their two supervisor tests
- [ ] Decide on the strict Subject-DN rebuild in cert-issuer `ca.rs`
- [ ] Test that `/issue` refuses `c:` extras
- [ ] Handler-level admin-action relay test in the observer (needs the `SignedAdminAction` struct)
- [ ] Cedar override-policy layer and the gitops policy path (proof of authorship for repo policies still open)
- [ ] OCSP-style revocation-status topics (custom Cassini request/reply, ledger-backed)
- [ ] Single-use `jti`; one token per cert
- [ ] Bootstrap cert permission set: least privilege?
- [ ] Configurable cluster domain (currently hardcoded `.svc.cluster.local`)
- [ ] Rename `polar-admin-grant`

## Dev environment and hygiene

- [ ] `IfNotPresent` + `:latest`: mitigated by `images.nu` dropping the node's cached copy (u7s only); proper fix is hash-based tags
- [ ] `my.env`: `GRAPH_PASSWORD` / `NEO4J_AUTH` after the password change; dev shell `BIND_ADDR`/`BROKER_ADDR` on 8080 collides with the Keycloak port-forward; `CASSINI_SHUTDOWN_TOKEN` is a dev constant
- [ ] ace <-> Neo4j connectivity (parked; ace is running)
- [ ] scheduler-observer `config_tests` isolation (`serial_test`)
- [ ] Back up the bootstrap and user keys and `secret/cert-issuer-ca`; delete `~/.config/polar-admin.old`

## Stale text

- [ ] Docs still say `git-observer.polar...`
- [ ] INFRA.md mentions `kube-observer-sa-token`
- [ ] `cert-issuer/server/src/main.rs` module comment says "v1 omits TLS"
- [ ] `src/deploy/` vs `infra/`: confirm `src/deploy/` is dead, then remove or mark
- [ ] README Keycloak walkthrough: confirm the HTTPS curl works against the port-forward

## Older backlog (from chats April to August; confirm which are still open)

- [ ] `infra/layers/3-workloads/agents/openapi/deployment.dhall` (the OpenAPI web frontend): still referenced, or safe to remove?
- [ ] `targets/sandbox/target.nu` is a placeholder; `flux/` and `networking/` charts not populated
- [ ] Slim `polar-init` (was blocked on nix-container-lib Shell selection)
- [ ] `maxLayers = 20` in package.nix files to speed up containerd image loading
- [ ] Empty `GITLAB_TOKEN` fails validation in some paths; handle gracefully
- [ ] Clippy pass across the workspace; dependency updates
- [ ] Cassini-broker actor tests (were deferred pending TLS)
- [ ] git-repo-observer / git-repo-consumer need real GitLab credentials in `git.json`
- [ ] `build-orchestrator` Dhall config wiring (`cyclops-k8s.dhall`, `cyclops-podman.dhall`)
