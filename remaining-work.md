# Remaining work

The single source of truth for what is left. Update it as items finish; do not rely on conversation memory.
Last updated: 2026-10-02.

## Done (2026-09-29 to 2026-10-02)

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
- [x] Configuration catalog, kube pair (design: `docs/architecture/config-catalog-design.md`; developer guide: `infra/INFRA.md`, rewritten for the catalog): `infra/config` (cluster, services, kinds, capabilities, facets, workloads), pure resolver `infra/resolve.nu` (explain, json, resources, diff, check with facet completeness), one generic `schema/agent.dhall`, `schema/resources.dhall`, `render-catalog.nu`, `compare-manifests.nu`; kube chart removed. Golden diff against the old render showed only the two intended deltas; `up.nu --dev` applied it to the live cluster and all pods came up
- [x] Configuration catalog, jira pair: both Rust agents on `polar-agent-builder` (full shutdown handshake, non-zero exits, bare single-wrapped wire format, hand-written backoff machinery and dead GitLab code removed, issues observer fails loudly with incremental pulls); `workloads/jira.nuon` plus per-target env in `local-minimal` and `local`; golden diff showed only `POLAR_CAPABILITIES` and `--check-dep-cert`; jira chart and the dead jira/kube render arms removed. Both targets render; on the cluster the processor is healthy and the observer starts correctly and fails loudly when the network is down, as designed

## Next: orchestration and Justfile

- [ ] Real fresh-cluster `up.nu` run end to end (provision + claim path has only been exercised on the resume path); confirm wait selectors (`job-name=...`), `polar-neo4j` statefulset, `pgrep -f svc/<name>`; record the user's `nu --version`
- [ ] `images.nuon`: add rows for git-observer/processor/scheduler, openapi-observer/processor, oci-resolver, scheduler-observer/processor, build-processor (attrs are in the `git-agents`, `openapi`, `scheduler`, `build-agents` recipes); then delete those four recipes
- [ ] Justfile: the `Agent containers` recipe (hardcoded `nix build ... -o result-*` list) duplicates the image table; replace with `nu infra/images.nu push --all` once the table is complete
- [ ] Justfile: remove stale dev-PKI recipes (`setup-cert-issuer`, `issue-dev-certs`, `run-jwks`, `run-cert-issuer`) and their comment block; fix leftover references (startup comment `just cluster-build-all && ...`, comment mentioning `just cluster-load-*`, orphaned "Pull and push the local-path provisioner" comment)
- [ ] Justfile: wrapper to provision further users signed with an existing admin cert (`admin-provision` is bootstrap-only)
- [ ] Justfile: Neo4j password helper; cluster status/health recipe; check `cluster-ctx`/`cluster-reset` against what `up.nu` expects
- [ ] Chromium variant of `user-cert-install` (`~/.pki/nssdb`; someone else tests it)
- [ ] `images.nu`: confirm `result/` is gitignored; check nothing else reads the old `result-*` symlinks (docs, other recipes); old digest entries accumulate in the node's image store
- [ ] Optional later: render injects image tags from `images.nuon` into the Dhall (single source of truth); content-hash tags would end the `IfNotPresent` + `:latest` hazard

## Configuration catalog (next slices)

Design: `docs/architecture/config-catalog-design.md`. Pattern per agent: migrate its Rust to the builder, add its catalog entry, golden-diff against the old render, remove its chart (and its arm in `render.nu`).

- [ ] Confirm kube-observer is doing its job on the cluster (logs; polar-init `--check-dep-cert` passed; no restarts after a few minutes) and kube-consumer has `POLAR_HEALTH_DEP_ENDPOINTS`
- [ ] Fresh-cluster apply of `kube-observer-resources.yaml` (the token Secret was already populated this time; ServiceAccount must exist first, `serviceaccounts.yaml` precedes it in `apply_order`)
- [ ] The first apply showed `clusterrolebinding/kube-observer-read configured`; expected to be an annotation-only difference. Confirm the next run says `unchanged`
- [ ] jira positive end-to-end test, when the Atlassian site is reachable: observer publishes projects/groups/users/issues, processor receives them (confirm its subscription log lines, which have not been seen yet) and writes the graph. A negative test with the placeholder `jira-secret` token (expect a loud 401 and a restarting Job) needs the network first. Set a real `jira_token` in the target secrets afterwards
- [ ] jira observer follow-ups: the user picker is queried with `query=''` (literal quotes) and appears to ignore `startAt`, so pages may repeat (consider `/rest/api/3/users/search`); the processor handles one issue per message, so batching needs a wire-type change; fix the stale comment in the ActorTerminated arm of `observe/src/supervisor.rs` ("typically after exhausting its backoff...") next time the file is touched
- [x] Rejuvenation handshake verified on a real cluster for kube-consumer (forced by setting POLAR_HEALTH_REJUVENATION_SECS above the cert lifetime: health check terminates, PrepareShutdown, unsubscribe, drain, graph flush, ProceedWithShutdown, infrastructure down, exit 1, Job restarts the pod). Not yet run live for jira-processor or kube-observer, which use the same builder path with their own supervisor halves
- [ ] Env precedence: a target `workloads.<name>.env` entry whose name the catalog also derives (seen with `POLAR_HEALTH_TICK_SECS`) is rendered twice and the later, derived value won silently (kubectl warns `hides previous definition`). Decide the precedence (the target should win) and make `resolve.nu check` fail on duplicate env names within a resolved workload
- [ ] kube agents: `main.rs` does `let _ = handle.await; Ok(())`, so a supervisor stop exits 0; `ProcessGroupChanged => todo!()` in both kube supervisors
- [ ] Builder: graph controller as infrastructure via `.with_graph_write()` (builder owns the spawn, `GraphSignal` wiring and flush), builder-owned shutdown handshake boilerplate; then migrate kube-consumer, jira-processor and credentials-agent to it
- [ ] credentials-agent (lives under `2-services`): the first real test of the escape hatch; decide which parts fit `agent` and which stay a custom chart
- [ ] Remaining agents (gitlab, git, scheduler, openapi, build-processor, resolver, ace); each needs its builder migration first
- [x] Rust builder uses capability names (`.with_graph_write()`, `.with_encryption()`, `.requires(Capability)`); `with_health_check` removed, health args derived from capabilities and `POLAR_HEALTH_DEP_ENDPOINTS`
- [x] Shared-name test (`names_match_the_deployment_catalog`) fails in both directions
- [x] Startup cross-check: catalog sets `POLAR_CAPABILITIES`; `AgentBuilder::build()` exits non-zero when requirements are not a subset (warns when unset)
- [ ] Decide baseline vs derived `encryption` and the polar-init escrow bootstrap (runs for every agent today) when mTLS integration through the builder lands; the builder spawns the CryptoActor, but TcpClient does not offload mTLS to it yet
- [ ] Remove the dead dep-endpoint literals and `HealthCheckArgs` from the remaining agents as each migrates to the builder
- [ ] Move other agents' RBAC (`ace-rbac`, etc.) into the `resources` facet; add further facets as needed (the resolver check enforces every capability answers every facet)
- [ ] Derive `apply_order` from the catalog (resources before the Job) instead of the hand-typed list in `target.nu`
- [ ] Replace the `render.nu` `match $chart_name` table (the remaining arms mostly pass the same four values) with data
- [ ] Pass `--cluster-domain` to polar-init from `cluster.domain`; derive polar-init env (now `Constants.commonClientEnv`) from the cassini capability
- [ ] Static check in `resolve.nu check`: health rejuvenation > expiry
- [ ] Remove the dead hard-coded endpoint fallbacks in the Rust agents once the env is always set; confirm `HealthCheckArgs` hard-coding was a convenience
- [ ] Gitignored local overlay for per-developer values; a second target's `config.nuon`
- [ ] `local` vs `local-minimal` drift: `polar_init_image`, `identity_lifetime_overrides`, and `local` lacks postgres/keycloak/ace
- [ ] Dev ergonomics: failed Job pods accumulate (restartPolicy Never + large backoffLimit); a cleanup recipe or a `podFailurePolicy`/TTL decision, tied to the no-graceful-exit audit

## Priority after the working cluster

- [ ] Builder rollout (`polar-agent-builder`) across all agents (done: kube-observer, kube-consumer, jira-observer, jira-processor)
- [ ] No agent exits gracefully: always non-zero so Jobs restart (audit every agent `main`; jira is clean, kube `main.rs` is not)

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
- [ ] Configurable cluster domain: `cluster.domain` in `infra/config/cluster.nuon` now drives catalog addresses; still hardcoded in the Dhall charts and not passed to polar-init (see catalog section)
- [ ] Rename `polar-admin-grant`

## Dev environment and hygiene

- [ ] **Cluster DNS (lives in `~/Documents/projects/nix-usernetes`, not Polar; do not change product code for it)**: the kubelet copies the node's search domain (`wifi.local.cmu.edu`) into every pod's `resolv.conf`, so each cluster-name lookup first tries `<name>.svc.cluster.local.wifi.local.cmu.edu` and times out against slirp4netns (10.0.2.3), roughly 4 s of startup delay in every agent; CoreDNS (static pod `coredns-u7s-node`, `/etc/coredns/Corefile`) forwards to 10.0.2.3 and times out on external names, so nothing outside the cluster resolves. Fix in the kubelet config (`/etc/kubelet/kubelet-config.yaml`: a `resolvConf` without the search line, and/or pod `dnsConfig` / `ndots`) and the Corefile forward target
- [ ] `IfNotPresent` + `:latest`: mitigated by `images.nu` dropping the node's cached copy (u7s only); proper fix is hash-based tags
- [ ] `my.env`: `GRAPH_PASSWORD` / `NEO4J_AUTH` after the password change; dev shell `BIND_ADDR`/`BROKER_ADDR` on 8080 collides with the Keycloak port-forward; `CASSINI_SHUTDOWN_TOKEN` is a dev constant
- [ ] ace <-> Neo4j connectivity (parked; ace is running)
- [ ] scheduler-observer `config_tests` isolation (`serial_test`)
- [ ] Back up the bootstrap and user keys and `secret/cert-issuer-ca`; delete `~/.config/polar-admin.old`

## Stale text

- [ ] Docs still say `git-observer.polar...`
- [ ] `cert-issuer/server/src/main.rs` module comment says "v1 omits TLS"
- [ ] `src/deploy/` vs `infra/`: confirm `src/deploy/` is dead, then remove or mark
- [ ] `DOCUMENTATION.md` points at `docs/devops.md`, which does not exist; it and the README still say `nu infra/render.nu <target> --apply` (render no longer applies; `up.nu` does) — point both at `infra/INFRA.md` and `up.nu`
- [ ] The empty `infra/secrets/` directory tree (from the first pipeline design) is unused; secrets are per target (`secrets.sops.yaml`, `secrets.nu`). Remove it
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
