<!--
  ACE Tool

  Copyright 2026 Carnegie Mellon University.

  NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
  FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
  EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
  PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
  MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
  TRADEMARK, OR COPYRIGHT INFRINGEMENT.

  Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
  for full terms.

  [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
  distribution.  Please see Copyright notice for non-US Government use and distribution.

  This Software includes and/or makes use of Third-Party Software each subject to its own license.

  DM26-0670
-->

# Key custody: a daemon per pod that holds the mTLS key

Status, 2026-10-27: cleared by the maintainer, with the four recommendations below. Nothing here is built.

## Why

An agent's Cassini identity is its mTLS client certificate, and the key behind it is a file. `polar-init` writes
`key.pem` into the volume it shares with the workload (`cert-issuer/client/src/output.rs`, `write_bundle`), and the
workload reads it: the Cassini client (`cassini/client/src/lib.rs:1312`, `PrivateKeyDer::from_pem_file`, then
`with_client_auth_cert` at `:1323`) and, for the credentials agent, its mTLS client to cert-issuer
(`cert-issuer/client/src/handshake.rs:110`, `reqwest::Identity::from_pem`). A compromised process can read the file and
be the agent from anywhere, for as long as the certificate lives, and nothing Polar does at the broker can tell the
copy from the agent. On record since 2026-10-08 (`remaining-work.md`, "Harden the mTLS client key").

What exists is the escrow key's custody, not this one: `polar-init` loads the escrow key into the kernel keyring and
leaves the workload only its number (`polar-init/src/handle.rs`, `polar_crypto::Custody`, `POLAR_ESCROW_KEY_HANDLE`).
Doing the same for the mTLS key ran into the kernel (probed on 7.2.9, 2026-10-08): an ECDSA P-256 key does not load
(`ENOPKG`), the kernel signs RSA with PKCS#1 v1.5 only where TLS 1.3 needs RSA-PSS, and rustls calls its signer
synchronously in the handshake. And the escrow key sits in the *user* keyring, which any process under the same UID
can reach, and which outlives the pod (`polar-kernel-keyring/src/lib.rs:62`, "A real, named security gap").

## The design (the maintainer, 2026-10-27)

**A custody daemon in every pod holds the pod's mTLS key, and does every signature with it.** The workload holds a
certificate and a socket, never the key. The idea is the kernel keyring's: usable but not readable. In user space it
has none of the kernel's limits, and it can do what the keyring cannot (decide, record, revoke).

- **A native sidecar.** An init container with `restartPolicy: Always`, declared before `polar-init`, so it starts first
  and runs for the pod's whole life; its own UID, no shared PID namespace, `PR_SET_DUMPABLE` off, its memory locked,
  under the builder's seccomp profile.
- **It makes the key.** At start it generates the keypair and the CSR. The private key never leaves it. `polar-init`
  takes the CSR (not a key) through issuance exactly as now, the token still only in `polar-init`, and gives the daemon
  the certificate and chain it was issued; the daemon checks the certificate is for its own key and judges it against
  the pinned CA, as `polar-init` does today (`judge_bundle`).
- **It uses the kernel keyring where the kernel can take the key, and its own memory where not** (the maintainer). In
  its *process* keyring, not the user keyring: only the daemon possesses it, so another process under the same UID
  cannot use it, and it is gone when the daemon ends; with kernel lockdown it is out of root's reach as well. Where the
  kernel cannot hold the key type, the daemon holds it, in locked memory, zeroed when it goes. Either way the workload
  sees the same thing.
- **The workload signs through a socket.** A Unix socket on a volume shared only by the pod's own containers. A rustls
  signer in `polar-custody`'s client (`rustls::sign::SigningKey`; not `polar-crypto`, which depends on the Cassini
  client that takes the signer) sends what a handshake needs signed and returns the signature;
  the Cassini client, the HTTP clients with an identity, and Cassini's own listener take it in place of a key file. A
  local round trip, synchronous as rustls wants, as a keyring signature is a syscall. The daemon answers only its own
  pod's containers (the socket's permissions, and `SO_PEERCRED` checked on each connection), and only signs: no
  operation gives the key out.
- **It is a Polar agent, with a Cassini client of its own.** It connects as the pod, signing its own handshake with the
  key it holds, so it needs no second identity.

**Immediate revocation (the maintainer).** A key that no one else holds can be revoked everywhere at once by destroying
it: no list for every verifier to fetch. The credentials agent already publishes the whole set of revoked serials,
signed, on `polar-authority-updates` (`AuthorityBody::Revoked`, `credentials-agent/common/src/authority_update.rs:87`),
and every builder agent holds it (`RevokedSerials`, `polar-agent-builder/src/authority.rs:224`). The daemon, a builder
agent, sees its own certificate's serial there, stops signing, and destroys the key (unlinked from its keyring, or
zeroed). Two things make it whole:
- *Sessions already open.* A TLS session, once up, does not use the key again, so the workload's open Cassini session
  outlives the key. The broker must also end the sessions of a revoked certificate: it knows each session's certificate
  (`cassini/broker/src/listener.rs:93`, `extract_peer_identity`) and does not consult revocation today.
- *Who may revoke.* Only the credentials agent, by the signature on the update, as every agent already checks it; a
  forged revocation would be a denial of service, not a takeover.

**What it changes elsewhere.**
- *The move to Deployments* (`runtime-attestation.md`, "Deployments, next"). Restarted in place, the pod restarts the
  daemon too, so each restart has a new key and a new certificate; nothing of the old key survives in the shared
  volume, because it never was there. The renewal rule's question (a pod restarted in place asking again under its UID)
  is the same as before, and is to be settled with the move.
- *The keyring's two recorded gaps* (`remaining-work.md`, "Key lifecycle in the user keyring"): keys shared by UID and
  keys left behind by a killed pod both go, for the mTLS key; the escrow key could move into the daemon later (below).
- *What runtime attestation assumed* (`runtime-attestation.md`, the table: "The private key lives in the kernel keyring,
  usable but not readable") becomes true of the daemon.

## What it protects, and what it does not

| Threat | Today | With the daemon |
|---|---|---|
| A compromised workload copies the key and is the agent elsewhere | possible: `key.pem` is in the shared volume | not possible: the key is never in the workload's reach |
| A compromised workload uses the key while it runs | possible | possible, as with the keyring; every signature is the daemon's to log, and revocation stops it at once |
| Another pod under the same UID on the node uses the key | not applicable (a file) | not possible: a process keyring, or the daemon's own memory |
| A killed pod leaves the key behind | the volume goes with the pod | nothing outlives the daemon |
| Root on the node | reads the file | reads the daemon's memory; with lockdown, not a key in the kernel keyring |
| Revoking a certificate | nothing stops a copy before it expires | the key is destroyed and the broker ends its sessions |

## Decided (the maintainer, 2026-10-27: all four as recommended)

1. **A revoked pod's daemon exits** once it has destroyed the key, so the pod restarts (in place) and asks again, and the
   credentials agent decides, by its own rules and the attestation agent's word, whether it is issued again.
2. **The broker ends a revoked certificate's sessions,** built with the daemon, holding the revoked set from the same
   signed authority updates.
3. **The escrow key moves into the daemon later,** as its own step after the mTLS key: one place for every key a pod
   holds, `CryptoActor` asking the daemon to decrypt as the workload asks it to sign.
4. **Cassini and the credentials agent carry the daemon** like every other pod; cert-issuer keeps its in-process key.

## Speed: Cassini is its first user (the maintainer, 2026-10-27)

Every TLS handshake signs once on each side: the client with its key, Cassini with its server key. So Cassini's daemon
signs once for every connection anyone in the cluster makes, and its load is the cluster's connection rate: every agent
registering at once when Cassini comes back (the reconnection storm), every rejuvenation, every reconnect after a
dropped session. What that asks of the design:
- *The algorithm before the keyring.* An ECDSA P-256 signature (today's key type) or an Ed25519 one is cheap; an RSA one
  is far slower, and slower still at 4096 bits. Where the kernel cannot sign a fast key type, the daemon holds the key in
  its own memory rather than switch the key to RSA to suit the kernel. The keyring is used only where it costs nothing
  in the algorithm.
- *No setup per signature.* One long-lived connection per process to its daemon, requests named so many are in flight
  at once, a compact fixed message (no JSON), and a daemon that signs concurrently. A signature costs a local round trip
  and the arithmetic, nothing more.
- *Fewer signatures.* TLS 1.3 resumption lets a client reconnect without a new signature on either side; whether the
  broker and the clients resume today is to be read before the build, and turned on if not.
- *A gate, measured.* The load harness (`broker-baseline.md`) gains a reconnection storm (every client at once, and a
  steady rate of reconnects), run with key files and with the daemon. The proposal, to be confirmed by the first
  measurement: the daemon adds no more than 100 µs to a signature at p99 under the storm, and Cassini's handshakes per
  second fall by no more than 10%. A build that misses it does not ship.

## Step 1 in detail (the maintainer, 2026-10-27)

- *A crate, `polar-custody`* (`src/agents/polar-custody`): the daemon binary and its library, the client side included.
- *Slots.* One key per slot, `client` and `server`, since a pod can hold both (Cassini, Neo4j). P-256, today's type.
  First in the daemon's locked memory; the process keyring next, through a load-and-sign path in `polar-kernel-keyring`
  where the kernel takes P-256, and memory where it does not (rcgen builds a CSR from a key it cannot see, so nothing
  else changes). Probed 2026-10-27: no kernel takes P-256, so memory it is (below, "The process keyring, probed").
- *The protocol.* rkyv frames on a Unix socket, each request with an id so many are in flight: `MakeCsr { slot, identity,
  names }`, `Install { slot, chain }` (the certificate must be for the slot's key and signed by the pinned CA, as
  `judge_bundle` judges a bundle), `Certificate { slot }`, `Sign { slot, message }`.
- *Who may ask what,* by `SO_PEERCRED` on each connection: `polar-init`'s UID makes CSRs and installs certificates; the
  workload's UIDs sign and read certificates; anyone else is refused.
- *TLS 1.3 handshakes only (the maintainer).* The daemon signs a message only if it is a TLS 1.3 CertificateVerify input:
  64 spaces, `TLS 1.3, client CertificateVerify` or `TLS 1.3, server CertificateVerify`, a zero byte, and a transcript
  hash of 32 or 48 bytes. A compromised workload can have it sign handshakes and nothing else, nothing in the agent's
  name; the rustls configurations that use it offer TLS 1.3 alone.
- *Hardening at start:* not dumpable (`PR_SET_DUMPABLE`), its memory locked, both logged where they cannot be had.
- *The workload side:* a blocking client with a few connections, and a rustls `SigningKey` that signs through it;
  wiring the Cassini client to it is step 2.
- *Not yet:* the Cassini client and revocation (step 5): the daemon starts before it has a certificate, so it is not a
  builder agent at first.

*Built (2026-10-27): step 1,* in `src/agents/polar-custody`. `tls.rs` (the TLS 1.3 CertificateVerify check), `key.rs`
(a P-256 key made and signed by `ring`, its CSR built by `cert_issuer_common::keypair::
csr_for_key`, split out of `generate_csr` for a key the caller cannot see), `custody.rs` (the slots, the roles, and the
judgement of a certificate installed: its key, its first name, valid now, no CA, signed by the pinned CA),
`protocol.rs` (rkyv frames, a request id each), `server.rs` (the socket at 0660, `SO_PEERCRED`, every request on its
own task, signatures under a shared lock), `client.rs` (a blocking client keeping up to eight connections, and the
rustls `CustodySigningKey` and `CustodyClientCert`), and the binary (`POLAR_CUSTODY_SOCKET`, `POLAR_CUSTODY_CA_CERT`,
`POLAR_CUSTODY_INSTALLER_UID`, `POLAR_CUSTODY_WORKLOAD_UIDS`; not dumpable and its memory locked, or logged).
Tests: the check, the key and its CSR, the roles, each refusal of a certificate, nothing signed before one is installed
or after a new key; through the socket, the whole flow, a caller turned away, each role alone, 400 signatures from 16
callers at once, and a real TLS 1.3 handshake with client authentication whose signature the daemon made; properties
(every handshake signed verifies; nothing else is ever signed; no request from any caller gets a key out). Mutations:
all on the rules caught. Not yet: the process keyring, and the workload, `polar-init` and the charts using it (steps 2
to 4).

*First measurement (2026-10-27, precisionws, release build, `tests/bench.rs`):* in process p50 102 µs, p99 113 µs;
through the socket, one caller, p50 108 µs, p99 208 µs (96 µs added at p99, inside the proposed 100 µs); eight callers
at once, p50 232 µs, p99 473 µs, 32,800 signatures a second. The socket costs little at the median; the arithmetic
costs most of it: the pure-Rust `p256` signs in about 100 µs, where `ring`, which rustls uses already, is several times
faster. A step to take before the speed gate.

*With `ring` (2026-10-27, same machine and bench, five runs).* `ring` needs nothing of the kernel: it is a user-space
library, its arithmetic in assembly and its randomness from `getrandom(2)`, and the 7.3 kernel with FIPS on
(`/proc/sys/crypto/fips_enabled` = 1) neither blocks nor changes it; every rustls handshake in Polar already signs
with it (the workspace `Cargo.toml` turns on `ring` for rustls, rustls-webpki and tokio-rustls). The daemon's key is
now a `ring::signature::EcdsaKeyPair` (`key.rs`); `p256` stays only in the tests, as a second implementation to verify
its signatures. The bench's in-process line is now `ring` too, as rustls signs for a workload today. In process p50
12 µs, p99 14 to 25 µs; through the socket, one caller, p50 16 to 17 µs, p99 21 to 36 µs (0 to 22 µs added at p99,
against 96); eight callers, p50 40 to 43 µs, p99 145 to 244 µs, 151,000 to 181,000 signatures a second, about five
times the first measurement. The daemon costs about 5 µs a signature at the median. Not a FIPS-validated module, and
neither was anything Polar signs with before; that is a separate question from custody (the kernel's FIPS mode covers
the kernel's own crypto, not a process's).

*What `ring` does not do, and the slot does instead.* `p256`'s signing key zeroed itself when dropped; `ring`'s
`EcdsaKeyPair` does not, nor does the PKCS#8 `Document` it is made from, so a key replaced by a new `MakeCsr` would
have left its scalar in the daemon's freed memory. Both are plain arrays (the scalar's limbs, the nonce key's digest,
the public point; no heap, no `Drop`), so `SlotKey` holds the pair boxed in a `MaybeUninit` and zeroes it in `Drop`,
and zeroes the `Document` once the pair is read from it (`key.rs`; tested, and the mutation that drops the wipe is
caught). What a stack copy made while generating leaves behind is as it was with `p256`: not reachable, and the
memory locked and the process not dumpable.

*Built (2026-10-27): step 2.* The workload takes the daemon's key in place of a key file, decided on the doc's own
terms and recorded here:
- *Which key.* `POLAR_CUSTODY_SOCKET` (the variable the daemon binds) or `TLS_CLIENT_KEY`, exactly one: neither or both
  is refused, naming both. `TLS_CLIENT_KEY` is now optional, and the rule is the `cassini-client` section's relation
  (`cassini_client::environment::read_client_key`), so the agent builder and the render-time check judge an
  environment as the client reads it. The two become `ClientKey::{File, Custody}` (`polar_custody::client`,
  re-exported by `cassini_client`).
- *The certificate stays a file,* `TLS_CLIENT_CERT`, as step 3 has it (the bundle becomes the certificate and `ca.pem`).
  The daemon judged the certificate it was given against its own key; one that differed would fail the handshake.
- *TLS 1.3 alone with the daemon's key,* in one place (`polar_custody::client::client_config`); with a key file, what
  rustls offers by default, as before. The test checks the server's reason (TLS 1.2 never offered), since the daemon
  refusing to sign a TLS 1.2 handshake would fail it too and hide a configuration that offered it.
- *A daemon that does not answer* fails the signature after 5 s (`client::TIMEOUT`, on the socket's reads and writes)
  rather than holding the thread rustls signs on; without it a stuck daemon would hold a tokio worker for good.
- *Who takes it:* the Cassini client (`cassini/client/src/lib.rs`, its TLS configuration in `pre_start`); the
  cert-issuer client's HTTP identity (`HandshakeClient::for_trust_mode_with_identity`), through a rustls
  configuration handed to reqwest whole (`use_preconfigured_tls`), since a PEM identity cannot carry a key the client
  never holds; and the credentials agent's own key, for both (`ConsumerSettings::own_key`).
- *Not yet:* cert-issuer's own key, which it makes and writes to `TLS_CLIENT_KEY` itself (it refuses the socket, so
  the catalog cannot give it one by mistake; step 4); Cassini's listener (step 4). The admin CLI and the dev setup keep
  key files: they are run by people, not in pods.
- *For step 3 to settle:* `polar-init`'s own Cassini session (the escrow bootstrap, `polar-init/src/main.rs`,
  `ensure_escrow_bootstrapped`) reads the same client variables (`TCPClientConfig::from_env`), so with the socket set
  it signs through the daemon too, and the daemon lets `polar-init`'s UID make CSRs and install, not sign. Decided (the
  maintainer, 2026-10-27): in a pod that runs the bootstrap, the chart lists `polar-init`'s UID among the workload's
  (`POLAR_CUSTODY_WORKLOAD_UIDS`) as well as the installer's. The daemon's roles stay as built; the catalog decides
  per pod, so a `polar-init` whose pod does not bootstrap can install but never sign.
- *Tests:* the daemon's key signing a handshake built by `client_config`; TLS 1.2 never offered with it; a key file as
  before; a silent daemon failing in time; every way of giving the two variables (unset, blank, a path), the section
  judging each as the client reads it; the credentials agent reading either; the HTTP identity presenting the daemon's
  certificate to an mTLS server. Mutations caught: TLS 1.2 offered, the timeout dropped, the two kinds swapped, both
  accepted, the HTTP identity left off. The section's check of an empty environment now reports four problems, not
  five: a relation runs once every variable parses, so the key is named as soon as the others are given.

*The process keyring, probed (2026-10-27, precisionws, 7.3.0, FIPS on).* The kernel cannot hold the daemon's key, and
turning ECDSA back on would not change that:
- *The probe.* A P-256 private key as PKCS#8, given to `add_key("asymmetric", ...)` in the probe's process keyring, is
  refused with `ENOPKG`, as on 7.2.9 (`remaining-work.md`, "Harden the mTLS client key"). An RSA-2048 control loads and
  signs (`KEYCTL_PKEY_SIGN`, `enc=pkcs1`). The RSA tests in `polar-kernel-keyring` pass.
- *Why, in the 7.3 source.* The PKCS#8 parser takes RSA alone (`crypto/asymmetric_keys/pkcs8_parser.c`,
  `pkcs8_note_algo`: anything but `OID_rsaEncryption` is `-ENOPKG`). The kernel's ECDSA verifies only
  (`crypto/ecdsa.c`: `.verify` and `.set_pub_key`, no `.sign`, and no private key in its context).
- *The ECDSA option is off,* and harmless here. Whitey's hand-curated 7.0.3 configuration turned off
  `CONFIG_CRYPTO_ECDSA`, with DH and ECDH, and recorded no reason (`/etc/nixos` `440f29c`). precisionws's 7.3
  configuration descends from it; the booted kernel has `ecc` and `ecdh_generic` and no ECDSA module. With the option
  on, the kernel could verify P-256 signatures and still neither hold nor sign with a P-256 key.
- *RSA, to suit the kernel, measured.* The kernel signs RSA-2048 in 2.9 ms at the median (p99 3.1 ms), and RSA-4096 in
  18 ms (p99 23 ms), against `ring`'s P-256 in 12 µs: 240 to 1,500 times slower. And it signs PKCS#1 v1.5 only, where
  TLS 1.3 needs RSA-PSS, which would take the raw private-key step with the padding done in user space. The decision
  above stands ("The algorithm before the keyring"): P-256, in the daemon's locked memory, zeroed when it goes.
- *What that leaves.* Root on the node can read the daemon's memory, where with lockdown it could not read a key in
  the keyring (the threat table above). The keyring is to be revisited when the kernel's PKCS#8 parser takes EC keys
  and its ECDSA signs. The escrow key, RSA-4096 and used to decrypt rarely, stays in the keyring, where the kernel can
  hold it.

## Step 3, the plan (2026-10-27, cleared by the maintainer)

What `polar-init` does today, read for this (`polar-init/src/main.rs`):
- It makes the key itself on both paths. On cert-issuer's, `issue_cert` calls `keypair::generate_csr` and sends the
  CSR. On the observer's, `polar_crypto::workload_certificates::request_certificate` makes the CSR in the pod and
  sends only the CSR, sealed with the token. Either way the key never leaves the pod, and only the CSR's origin
  changes.
- It writes `cert.pem`, `key.pem` and `ca.pem` (`cert-issuer/client/src/output.rs`, `write_bundle`). It judges what the
  observer hands over against the pinned CA, including that `key.pem` is the certificate's key (`judge_bundle`).
- It keeps a valid bundle already in the emptyDir (`keeps_existing_bundle`).
- It asks for P-256 everywhere: all 27 `--cert` flags in `infra` say `ecdsa-p256`.
- Graph writers ask for two client certificates: Cassini's (`tlsPath`) and Neo4j's (`neo4jClientCertPath`,
  `build-processor/processor.dhall:83`). Neo4j's server asks for one (`neo4j/statefulset.dhall:191`). neo4rs and
  Neo4j read key files, so those keep files (`remaining-work.md` already records neo4rs).

The plan:
1. *A `--cert` says when its key is the daemon's.* Decided (the maintainer): the key-algorithm field reads `custody`
   (`client:/etc/tls:custody:`). That field already says who makes which key, and the daemon makes a P-256 key in the
   slot of the certificate's type (client or server). At most one such certificate per type, since there is one slot
   each, and a second is refused at start. `POLAR_CUSTODY_SOCKET` must then be set for `polar-init`, or it stops at
   start, naming both.
2. *Per such certificate:*
   - one `MakeCsr` for the identity and the names, waited for while the socket is not there yet (the sidecar
     starting) and stopped at a refusal;
   - that CSR through whichever path the pod uses, retried as now. On the observer's path,
     `request_certificate_with_csr`, with `request_certificate` built on it;
   - the certificate judged as now, but against the CSR's key, not a key file (`output::judge_certificate`, with
     `judge_bundle` built on it);
   - `Install` into the daemon, which judges it again;
   - then `cert.pem` and `ca.pem` written, and any `key.pem` removed. The daemon holds the certificate before the
     workload can read it.
3. *No reuse of an old bundle for it.* The key is new at every start (`deployments.md`, item 4), so
   `keeps_existing_bundle` is skipped for a daemon's certificate. It stays for the key files until the move.
4. *The escrow bootstrap's session* signs through the daemon. Its UID is among the workload's (decided above), and with
   the socket set and `TLS_CLIENT_KEY` unset it reads `ClientKey::Custody`.
5. *Tests first:*
   - the flag parsed, and one per type;
   - against a daemon in a test and the observer and cert-issuer mocked, on both paths: no `key.pem`, `cert.pem` and
     `ca.pem` written, and the daemon holding the certificate;
   - a certificate for another key, or under another CA, refused before anything is written;
   - a socket that appears late, waited for;
   - a refusal, stopped at.

*Open, beyond this step: the renewal rule before the charts.* With the daemon, a pod whose init container runs again
under its UID (a node restart recreating its sandbox, the case of 2026-10-25) has a new key and must be issued again.
The credentials agent refuses it while its first certificate is valid (`AlreadyHasValidCertificate`), until the
renewal rule is built (`deployments.md`, items 1 and 2, its build step 2). Decided (the maintainer): the renewal rule
is built after step 3 and before step 4 gives pods a daemon, so no pod runs with a daemon it cannot be issued for
again after a restart.

## Build order, each tests first

1. The daemon: key generation (process keyring or memory), the CSR, taking the certificate and judging it, signing,
   the socket and its caller check, and requests in flight concurrently. Properties: a signature verifies under the
   certificate it holds; no message gets the key out; a caller that is not the pod's is refused. A benchmark of a
   signature through the socket against one in process, from the start.
2. The workload signer in `polar-custody` (rustls), and the Cassini client and the HTTP identities taking it in place of
   `TLS_CLIENT_KEY`.
3. `polar-init` taking the daemon's CSR and handing it the certificate; the bundle in the shared volume becomes the
   certificate and `ca.pem` alone.
   Then the renewal rule (`deployments.md`, build step 2), before any pod has a daemon.
4. The catalog and the charts: the sidecar in the `agent` and `service` kinds, its socket volume, its UID; Cassini's
   listener taking the signer.
5. Revocation: the daemon on its own serial, its exit, and the broker ending revoked sessions.
6. The speed gate: the reconnection storm in the load harness, with key files and with the daemon.
7. On a fresh sandbox-local: no `key.pem` in any pod; a revoked agent's session ends and its key is gone.
