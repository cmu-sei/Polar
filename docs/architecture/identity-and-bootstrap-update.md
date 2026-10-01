# Identity, CA trust, and admin bootstrap — update (2026-09-29)

Supplements `credentials-agent-architecture.md`. Sections marked **OPEN** were decided in
the "Debugging git-repo-observer initialization" chat and are NOT recorded here; fill them in
rather than trusting anything inferred.

## 1. Workload identity (built)

- One ServiceAccount per workload, bare `<service>` name (no `-sa`). Nothing runs as `default`.
- Cert SAN: `<sa>.<ns>.serviceaccount.cluster.local`, derived from the projected token `sub`
  via `normalize_identity`; cert-issuer byte-matches it against the CSR SAN.
- `render.nu` enforces at render time: ServiceAccount uniqueness (emits `serviceaccounts.yaml`),
  cert-issuer TLS identity == callers' URL host, CA-provisioning consistency.
- Neo4j runs as SA `graph` (ns `polar-graph`) and gets its cert from `polar-init`.
- Certs are issued by the `polar-init` init container; the main container never holds the token.
- Tracked: single-use `jti` and one token per cert; configurable cluster domain.

## 2. cert-issuer (built)

- TLS-only listener (client cert optional). Peer identity comes only from the handshake.
- `POST /issue` (bearer token), `POST /validate`, `GET /ca` (unauthenticated),
  `POST /internal/issue` (peer identity must be in `internal_callers`; asserted identity must
  match CSR SAN). `internal_callers` is currently empty; target value is
  `credentials-agent.polar.serviceaccount.cluster.local`.
- CA modes, explicit in config, never inferred:
  - `SelfBootstrap`: generate on first start (exact 0600 key).
  - `Provisioned`: load only, never generate (key mode `& 0o027 == 0`, 0440 ok for fsGroup 1000).
- `polar-admin-grant bootstrap-ca` (needs a kube-admin credential) creates Secret
  `cert-issuer-ca` (ns `polar`) and ConfigMap `polar-ca` (ns `polar`, `polar-graph`). Create-only.
- Client trust: provisioned mode mounts `polar-ca` and verifies from the first byte
  (`POLAR_INIT_CA_CERT`, `CREDENTIALS_AGENT_CA_CERT`). Fetch mode does one unverified
  `GET /ca` then pins. Accepted trade-off: same trust as in-cluster DNS. Only someone with
  kube-admin can do the provisioned path, which is why it is preferred.

## 3. Cassini client convention (built)

Messages sent while `Unregistered` are dropped. Callers subscribe from `ClientEvent::Registered`.
`CryptoActor` now subscribes on `CryptoMessage::ClientRegistered`, sent by `polar-init`
(polling `GetRegistrationId`) and by the agent-builder on `Registered`.
Known hazard: builder skips the send if `Registered` fires before `state.crypto` is set.
The builder path is untested in a cluster.

## 4. Health and rejuvenation (confirmed behavior)

Failed graph canary -> healthcheck actor dies -> supervisor exits the agent -> Job restarts
the pod -> `polar-init` issues fresh certs -> ledger replays from `events.jsonl` on the PVC.
credentials-agent must be restartable on its existing storage mount with new mTLS certs and
rebuild state from the ledger.

## 5. Target path A -> B (user certificates)

1. Bootstrap admin created (through the observer, with a Kubernetes credential; once only).
2. Bootstrap admin provisions a normal admin (`provision-account`).
3. Normal admin's cert is imported into a browser.
4. Browser reaches Neo4j Browser over mTLS (`client_auth=REQUIRE`, chain-only check).
5. Neo4j password changed interactively from `neo4j` to `somepassword`
   (unblocks `kube-consumer`, `jira-processor`). `NEO4J_AUTH` is ignored by the custom image;
   do not seed the password.

Then: `render.nu` stops applying; a separate orchestrator script brings the cluster up,
pauses at manual steps, and prints status along the path.

## 6. Accounts, claims, admin bootstrap (recovered from the 2026-09-25 session summary; user answers 2026-09-29)

**Account** (first-class resource on the credentials-agent ledger):
- UUID is authoritative and lives in the cert. It is generated server-side; users never supply it.
- username, email (admin-authoritative, not user-overridable), plus optional first/last name, title,
  phone (user may supply via CSR; admin can set defaults; CSR value wins).
- Before a UUID exists, (username, email) together identify the account, so it can get or find a UUID.
- Accounts exist so identity survives a lost or expired cert while a new one is issued (needs admin).
- Possession of the private key is identity. No logout, no self-service recovery.

**Provisioning and claim (IdP and native converge):**
- Admin provisions first: `AccountProvisioned { account_id, username, email, permissions,
  provisioned_by, timestamp, reason, name/title/phone defaults }`. Ledger state keeps
  `active_provisioned_accounts` keyed on (username, email).
- User then runs the CSR flow. Native user: the requested (username, email) must match what the admin
  provisioned; no out-of-band secret. IdP user: same, with the IdP token as the credential.
  Result is a cert with identity = account_id.
- The claim is not open to renewal. Renewal, or replacing a lost cert, needs an admin: the admin
  re-enables the account (expiring the old credential first: NOT BUILT), then the user reruns the tool.
- Deferred: user-initiated request that an admin discovers in the admin tool.

**Bootstrap admin:**
- Dedicated SA `polar-admin-bootstrap` in ns `polar-bootstrap`, zero RBAC, audience `polar-cert-issuer.local`.
- Once only: refused forever after the first success (ledger check).
- `polar-admin-grant bootstrap` mints the K8s token, builds the CSR, calls observer `/bootstrap`, writes
  the cert, validates it on disk (byte identity + keypair match), warns loudly that losing it means a
  cluster rebuild.
- K8s tokens from `kubectl create token` lack pod UID: allowlist `sub` == that SA, then use `jti` as
  instance binding (`consume/src/issuance.rs` ~lines 119-124).
- Same mechanism as the IdP flow: the observer forwards blindly; the consumer decides.

**Still to build:** `AccountProvisioned` event; `active_provisioned_accounts`; ledger
`CheckProvisioning` / `AppendAccount`; observer `/bootstrap` and `/claim`; `bootstrap` and `create-user`
subcommands; `user-cert-fetcher` claim flow; the SA and namespace manifests; credential expiry;
OCSP-style revocation-status topics (custom Cassini request/reply, ledger-backed); Subject-DN merge
in cert-issuer (in progress earlier).

**OPEN:** publisher authorization on `credentials-grants`, `credentials-revocations`,
`credentials-account-provisioning`; who supplies the bootstrap admin's username/email.

## 7. Manual steps log (until the orchestrator exists)

kube-admin credential for `bootstrap-ca`; browser cert import; Neo4j interactive password
change; sops keys.

## 8. Stale in existing docs

Docs still say `git-observer.polar…`; INFRA.md mentions `kube-observer-sa-token`;
`cert-issuer/server/src/main.rs` module comment says "v1 omits TLS"; tool is still named
`polar-admin-grant`.

## 9. Authorization of admin actions (decided 2026-09-29)

- **Bootstrap is consumer-internal.** The bootstrap admin is provisioned by the credentials-agent consumer from the
  `/bootstrap` request (K8s token check + once-only flag). Nobody publishes an `AccountProvisioned` for it. The
  bootstrap admin's only job is to provision the normal admin; its username/email are supplied by the tool.
- **The consumer is the gate**, not the broker or the topic. Everything that reaches the credentials agent becomes a
  ledger event first, then is processed.
- **Authorship is proven by signature, not by transport.** Each admin action (provision, grant, revoke, policy
  update) carries a signature made with the author's cert key. The consumer verifies it against the author's cert
  and looks up the author's effective permissions in its own ledger state. `published_by` and topic identity are not
  used for authorization. (sops gives confidentiality only; encrypting to a public key proves nothing about the author.)
- **Global sequence.** Signed admin actions carry a sequence number from one global counter. The consumer refuses a
  used or stale number. The admin tool asks the credentials agent for the next number, signs, publishes, and retries
  on conflict. Chosen because the ledger is meant to be an auditable, strictly ordered log.
- **Admin is a permission carried in the user's cert**, sourced from the account's `permissions` at provisioning.
  The ledger's `AccountCertificateRecord` currently drops permissions and has no revoked flag: both must be added.
- **Effective permissions = cert baseline + policy overrides.** Each cert permission acts as a `permit` for that
  username. Override policies (Cedar text) are added to the same set; any matching `forbid` wins. A demotion takes
  effect immediately, without waiting for cert expiry.
- **Policy overrides** live in the gitops repo as documents. The scheduler agent serializes them and forwards them
  as one variant of a tagged payload (policy update is its own type); it never interprets them. The credentials
  agent parses and validates them; on a malformed policy it replies to the scheduler with the consumer's error so
  the scheduler can mark that repo source invalid. Policies name users by **username**.

**Still open:** what proof of authorship covers policy updates that arrive via the repo (author's cert signature vs
repo access control / signed commits); the bootstrap cert's permission set (least privilege?); wire format of the
signed envelope.
