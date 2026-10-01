# Identity, CA trust, and admin bootstrap — update (2026-10-01)

Supplements `credentials-agent-architecture.md`. Sections 1–4 describe the workload-identity foundation; sections 5–10
describe the account, claim and admin-action layer, which is built and exercised end to end (bootstrap admin ->
provisioned admin -> user cert -> browser -> Neo4j).

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
  match CSR SAN). `internal_callers` is `credentials-agent.polar.serviceaccount.cluster.local`: the consumer calls it
  to sign account certs after verifying a claim. No bearer token is involved.
- Lifetimes: `default_lifetime` (client/workload, 3600 s), `server_lifetime`, `user_lifetime` (any cert for an account;
  1 year by default) and per-identity overrides, which win over all of them.
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

## 5. Path A -> B (user certificates): achieved 2026-10-01

1. `just admin-bootstrap`: bootstrap admin created through the observer with a Kubernetes credential (once only).
2. `just admin-provision <user> <email>`: the bootstrap admin provisions a normal admin.
3. `just admin-claim <user> <email>`: the admin claims the account (ECDSA P-256, 1-year cert).
4. `just user-cert-install <user>`: the cert and the Polar CA are imported into the browser.
5. The browser reaches Neo4j Browser over mTLS (`client_auth=REQUIRE`) at `https://localhost:7473`.
6. Neo4j password changed interactively from `neo4j`; this unblocks `kube-consumer` and `jira-processor`.
   `NEO4J_AUTH` is ignored by the custom image; do not seed the password.

Neo4j trusts only the Polar CA for client certs on both connectors: the JVM keystore is built from `polar-internal-ca`
alone (it used to be the full JDK `cacerts`, which let any public-root-signed cert past the https TLS gate).

Next: `render.nu` stops applying; a separate orchestrator script brings the cluster up, pauses at manual steps, and
prints status along the path.

## 6. Accounts, claims, admin bootstrap (built 2026-09-30 to 2026-10-01)

**Account** (first-class resource on the credentials-agent ledger):
- UUID is authoritative and lives in the cert. It is generated server-side; users never supply it.
- username, email (admin-authoritative, not user-overridable), plus optional first/last name, title,
  phone (user may supply via CSR; admin can set defaults; CSR value wins).
- Before a UUID exists, (username, email) together identify the account.
- Accounts exist so identity survives a lost or expired cert while a new one is issued (needs admin).
- Possession of the private key is identity. No logout, no self-service recovery.

**Provisioning and claim (two phases, observer `/account`):**
- An admin provisions first (`provision-account`): username, email, permissions (`p:`), capabilities (`c:`), reason.
  The account is then pending.
- Begin: the claimant (Native: username + email, which must match the provisioning; Bootstrap: a Kubernetes SA token)
  receives `Offered { account_id }`.
- Complete: a CSR bound to that `account_id` returns `Issued { certificate_pem, ca_chain_pem }`. Cert identity =
  `account_id`; the Subject DN carries `uid` (username) and `mail` (email), set by cert-issuer from the verified account
  subject and never from the CSR. There is no out-of-band secret for Native claimants.
- `claim` generates the keypair locally (the key never leaves the machine) and writes it 0600 before Complete.
  The claim key is **ECDSA P-256**: browsers (Chrome/NSS) reject Ed25519 client certs. The bootstrap cert stays Ed25519.
- Lifetime: `user_lifetime` in the cert-issuer config (dhall `userLifetimeSecs`, default 31536000 = 1 year), applied
  to any cert issued for an account; a per-identity override still wins. Workload and server lifetimes are separate.
- **No self-renewal** (decision 2026-09-30): a new cert always involves an admin.
- Admin `reissue` is designed, not built: revoke the old cert, return the account to pending with the same
  `account_id`, holder reruns `claim`. Until then, provisioning an existing username is refused (`AccountAlreadyExists`);
  in dev the answer is a cluster rebuild.
- Deferred: a user-initiated request that an admin discovers in the admin tool.

**Bootstrap admin:**
- Dedicated SA `polar-admin-bootstrap` in ns `polar-bootstrap`, zero RBAC, audience `polar-cert-issuer.local`.
- Redemption is once only, refused forever after the first success (ledger check). The resulting bootstrap cert may
  provision exactly one account (`BootstrapCertificateSpent` afterwards); reserved usernames are refused.
- `polar-admin-grant bootstrap` mints the K8s token, builds the CSR, claims via the observer, writes the cert, validates
  it on disk (byte identity + keypair match), and warns loudly that losing it means a cluster rebuild.
- K8s tokens from `kubectl create token` lack pod UID: allowlist `sub` == that SA, then use `jti` as
  instance binding (`consume/src/issuance.rs` ~lines 119-124).
- Same mechanism as the IdP flow: the observer forwards blindly; the consumer decides.

**Remaining work:** the admin `reissue` action; expiry check on admin actions; OCSP-style revocation-status topics
(custom Cassini request/reply, ledger-backed); removing the old blind path (the GRANTS / REVOCATIONS /
ACCOUNT_PROVISIONING subscriptions); a strict Subject-DN rebuild decision in cert-issuer `ca.rs`; a test that `/issue`
refuses `c:` extras.

**Closed:** publisher authorization on the old topics is moot for admin actions, because the consumer is the gate
(section 9). The bootstrap admin's username/email are supplied by the tool.

## 7. Manual steps log (until the orchestrator exists)

- kube-admin credential for `bootstrap-ca` (create-only; refuses if `secret/cert-issuer-ca` exists, so a rebuild means a
  cluster reset).
- After a rebuild the new CA invalidates all old admin material: move `~/.config/polar-admin` aside first
  (`bootstrap` refuses existing outputs).
- Browser import of the user cert and the CA: `just user-cert-install`.
- Neo4j interactive password change (`kube-consumer` and `jira-processor` fail to authenticate until it is done; avoid
  repeated wrong-credential retries, which trip Neo4j's per-user failed-login rate limit).
- sops keys.
- Back up the bootstrap and user keys and `secret/cert-issuer-ca`; losing the bootstrap material means a rebuild.

## 8. Stale in existing docs

Docs still say `git-observer.polar…`; INFRA.md mentions `kube-observer-sa-token`;
`cert-issuer/server/src/main.rs` module comment says "v1 omits TLS"; the tool is still named
`polar-admin-grant`; the README's Keycloak walkthrough talks plain HTTP to cert-issuer, which is TLS-only (verify).

## 9. Authorization of admin actions (decided 2026-09-29)

- **Bootstrap is consumer-internal.** The bootstrap admin is provisioned by the credentials-agent consumer from an
  observer `/account` claim carrying a K8s credential (token check + once-only flag). Nobody publishes an
  `AccountProvisioned` for it. The bootstrap admin's only job is to provision the normal admin; its username/email are
  supplied by the tool.
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

**Built (2026-10-01):** the consumer verifies, in order: author serial lookup in the ledger, revocation, signature
(Ed25519 or ECDSA P-256), required capability, replay, and for the bootstrap cert the one-shot `provisionings_made`
flag. The action and its effect are recorded in one atomic ledger write (`RecordAdminAction`). Refusal-to-HTTP mapping
at the observer: Recorded 200; UnknownAuthor/BadSignature 401; AuthorRevoked/MissingCapability/ReservedUsername/
BootstrapCertificateSpent 403; MalformedPayload 400; Replay/AccountAlreadyExists/AccountAlreadyPending 409;
NotYetSupported 501; Failed 503; timeout 504. Wire format: `SignedAdminAction` in `credentials_agent_common::admin_signing`.

**Not built:** an expiry check on the author cert (the consumer appears not to check validity period on admin actions;
verify); Cedar override policies and the gitops policy path; the admin `reissue` action (section 6).

**Still open:** what proof of authorship covers policy updates that arrive via the repo (author's cert signature vs
repo access control / signed commits); the bootstrap cert's permission set (least privilege?).

## 10. Observer routes and the admin CLI (built)

- The observer is a blind HTTP-to-Cassini request/reply relay. It decides nothing; the consumer is the gate.
  Routes: `/issue`, `/identity`, `/account` (two-phase claim), `/admin-action` (signed admin actions, fixed 100 s ceiling).
- `polar-admin-grant` (name still to change) talks to the observer over HTTP. `--observer-url` is always explicit;
  there is no default in product code (the Justfile wrappers default to the dev port-forward, `http://localhost:8081`).
- Subcommands: `bootstrap-ca` (kube-admin credential, no observer), `bootstrap`, `claim`, and the signing path
  (`provision-account` and other admin actions, signed with the author's cert key).
- Dev wrappers: `just admin-bootstrap`, `admin-provision <user> <email>`, `admin-claim <user> <email>`,
  `user-cert-install <user>`.
