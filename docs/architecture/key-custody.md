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

Status, 2026-10-27: cleared by the maintainer, with the four recommendations below. Built: steps 1 to 5, each recorded
in its section below; step 6, the speed gate, closed; step 7 done on a fresh sandbox-local (`u7c`), revocation seen end
to end.

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
   holds, `CryptoActor` asking the daemon to decrypt as the workload asks it to sign. Planned 2026-10-27 ("The escrow
   keys into the daemon").
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

*Built (2026-10-27): step 3,* as planned:
- *`polar-init`* (`polar-init/src/main.rs`):
  - `KeyMaker::{Here, Custody}` parsed from the key-algorithm field;
  - `custody_for` says before anything waits whether a socket is needed, and refuses two of a type;
  - `of_the_daemon` waits while the daemon cannot be reached and stops at a refusal;
  - `custody_csr`, then the path's own retries, then `judged_for_csr` and `install_and_write`;
  - `issue_custody_cert_from_cert_issuer` and `issue_custody_cert_via_observer`, neither keeping an old bundle.
- *`polar-crypto`:* `request_certificate_with_csr`, with `request_certificate` built on it.
- *The cert-issuer client* (`output.rs`): `judge_certificate`, a certificate judged against its CSR's key by the same
  judgement as a bundle's, and `write_certificate`, the certificate and the CA with any `key.pem` removed.
- *`POLAR_CUSTODY_SOCKET`* is read into `InitSettings`; it is the Cassini client's variable, already in
  `polar-init`'s snapshot.
- *Tests:*
  - the flag;
  - the socket rule and one per type;
  - both paths against a daemon in the test: the daemon holds the certificate, `ca.pem` is the pinned CA, and an
    earlier `key.pem` is gone;
  - a certificate under another CA neither installed nor written;
  - a bundle from before not kept;
  - a socket that comes late, waited for;
  - a daemon that refuses, stopped at;
  - a CSR made elsewhere sent as given;
  - the CSR judgement's cases and the keyless writer.
- *Mutations caught:* an old bundle kept, an unreachable daemon taken as final, two of a type let through.

## The neo4rs patch, the plan (2026-10-27, cleared by the maintainer)

A graph writer presents its one client certificate to Neo4j through neo4rs, which reads the key from a file. With the
daemon there is none. The maintainer: patch neo4rs, carried on our own fork for the time being.

What is there, read for this:
- *Polar's neo4rs:* `neo4rs = { git = "https://github.com/neo4j-labs/neo4rs", tag = "v0.9.0-rc.8" }` (workspace
  `Cargo.toml`), commit `1b92c9b`, used by `lib` and eight agent crates. Polar builds its connection in one place
  (`lib/src/graph/controller.rs`, `neo_config_for`), from `GraphSettings` (`lib/src/graph/environment.rs`): with
  `GRAPH_CLIENT_CERT`, `GRAPH_CLIENT_KEY` and `GRAPH_CA_CERT` it calls `with_mutual_tls_validation(ca, cert, key)`.
- *neo4rs's TLS:*
  - `ConnectionTLSConfig` (`lib/src/auth/mod.rs`: `None`, `ClientCACertificate`, `NoSSLValidation`, `MutualTLS`, all
    file paths) is turned into a rustls `ClientConfig` in `ConnectionInfo::tls_connector`
    (`lib/src/connection.rs`), which opens the key file;
  - there is no way to hand it a configuration;
  - the `+ssc` schemes replace whatever was given with a no-validation one.
  - Its rustls is 0.23 with `ring`, the one rustls in Polar's tree, so a `ClientConfig` Polar builds is the type
    neo4rs uses.
- *Neo4j's side:*
  - Bolt requires TLS and a client certificate (`targets/*/conf/neo4j-ssl.conf`: `server.bolt.tls_level=REQUIRED`,
    `dbms.ssl.policy.bolt.client_auth=REQUIRE`), and sets no `tls_versions`;
  - the image is Neo4j 2025.08.0 built from source (`nix-neo4j`, though `images.nuon` labels it `5.26.2`), whose
    policy defaults to TLS 1.2 and 1.3 (`SslPolicyConfig` in its `neo4j-configuration` jar), so the daemon's TLS 1.3
    handshake fits;
  - Polar connects with `bolt+s` (`infra/config/capabilities.nuon`).
- *The precedent:* forks live under `github.com/daveman1010221` and are pinned by commit, each with a comment in the
  workspace `Cargo.toml` saying why (`rsa`, `keyutils-raw`).

The plan:
1. *The fork.* `daveman1010221/neo4rs`, forked from `neo4j-labs/neo4rs`, a branch `polar` from tag `v0.9.0-rc.8`
   (since moved to `v0.9.0-rc.10`, below),
   one commit. The patch is shaped so it could go upstream as it is. *Done (2026-10-27):* forked with `gh` (the
   maintainer's choice), the commit `a038df3` on `polar`, pushed. The neo4rs library's 286 tests pass, four of them
   new. Clippy reports the same four lints at the tag as with the patch: none added.
   *Moved to the latest release (the maintainer, 2026-10-27):* the fork is to follow neo4rs's latest version, not the
   tag Polar happened to pin. The patch is carried onto `v0.9.0-rc.10` (2026-06-11), branch `polar-v0.9.0-rc.10`,
   commit `4117f5a`, pushed. `rc.10` reworked the connector, so the change was made again on its code, the same
   change. Its 294 library tests pass, the four new ones among them, and clippy reports the same three lints as at
   `rc.10` itself. Polar compiles against it unchanged. `main` is nine commits past `rc.10`, unreleased (among them
   fixes to Bolt chunking and pooled connections, and rustls-platform-verifier in place of rustls-native-certs); the
   fork moves when a release does. The first branch, `polar` (`a038df3`), is left as it was and is not used.
2. *The patch:*
   - a fifth variant, `ConnectionTLSConfig::Rustls(RustlsConfig)`, a newtype over `Arc<rustls::ClientConfig>` that
     compares by pointer, since the enum is `PartialEq` and a rustls configuration is not;
   - `ConfigBuilder::with_rustls_config(Arc<ClientConfig>)`;
   - `tls_connector` uses it as given;
   - under the `+ssc` schemes it is also used as given: the caller's configuration already says how the server is
     verified, and the downgrade exists because the built-in variants know nothing else. Plain `bolt://` has no TLS,
     as now.
   - Tests in neo4rs: the builder sets it; for `bolt+s`, `neo4j+s` and `+ssc` the configuration the connector uses
     is the very one given; plain `bolt` takes none.
3. *Polar:*
   - the workspace's neo4rs moves to the fork, pinned by commit, with the comment;
   - `GraphTls::Mutual`'s key becomes `ClientKey`: `GRAPH_CLIENT_KEY`, or the custody socket, which the graph reads
     through the Cassini client's declaration (`cassini_client::environment::CUSTODY_SOCKET`), as `polar-crypto` reads
     `TLS_CLIENT_CERT`;
   - with `GRAPH_CLIENT_CERT` and `GRAPH_CA_CERT` set, exactly one of the two keys, as for Cassini; without
     `GRAPH_CLIENT_CERT`, the socket says nothing to the graph;
   - `neo_config_for` builds the daemon's configuration with `polar_custody::client::client_config` (TLS 1.3 alone)
     and hands it over with `with_rustls_config`. A key file goes the way it goes now.
4. *Tests first, in Polar:*
   - the graph environment's every combination, the section judging each as the controller reads it;
   - end to end: neo4rs, configured for the daemon, connecting to a TLS 1.3 server that demands a client certificate,
     which sees the daemon's certificate. The connection is lazy, so a query forces it; the server is a stand-in, not
     Neo4j, since container tests cannot publish ports on this host.
   - A real Neo4j is seen with step 7, on a fresh cluster.
5. *Not changed:* the charts (step 4 points `GRAPH_CLIENT_KEY` at the daemon), `polar-test-graph`, and the `ace`
   job's own certificate.

## Degraded operation, the plan (2026-10-27, items 1 to 3 cleared by the maintainer, and built)

The maintainer: Polar may use its own builds of Neo4j and neo4rs, but there must be a degraded way to run for a
deployment that cannot use what we build. What the daemon's whole path rests on, and what each piece's absence costs:

| Piece | Ours | Without it |
|---|---|---|
| The custody daemon (`polar-custody`, a native sidecar) | the key never in a file | key files, as before custody: `TLS_CLIENT_KEY`, the key readable in the pod's volume |
| neo4rs taking a rustls configuration (`daveman1010221/neo4rs`, branch `polar`) | a graph writer shows Neo4j the daemon's key | that pod on key files, or the tunnel below |
| Neo4j's Bolt offering TLS 1.3 (our 2025.08 build does by default) | the daemon signs TLS 1.3 handshakes only | set it in the policy (below); a Neo4j that cannot is reached with key files |

The plan:
1. *Key files stay a whole, supported mode, chosen per pod.* Every client already takes either `TLS_CLIENT_KEY` or
   `POLAR_CUSTODY_SOCKET` (step 2), and `polar-init` writes a key for any `--cert` that is not `custody` (step 3), so a
   pod without the daemon runs exactly as Polar ran before custody. What it gives up is stated where it is chosen: the
   key is in the pod's volume, readable by the workload, so a compromised workload can copy it and be the agent
   elsewhere until its certificate expires or is revoked (the table in "What it protects").
2. *The choice is the target's, rendered.* Step 4 gives the target a `custody` setting, on by default. Off renders
   every pod without the sidecar, its `--cert` as `ecdsa-p256`, and its key variables as files. A deployment that
   cannot run our daemon image, or uses stock neo4rs, sets it off. It can be per workload as well, so that only the
   graph writers fall back if only neo4rs is the problem.
3. *Neo4j's TLS versions said, not assumed.* The Bolt policy in `targets/*/conf/neo4j-ssl.conf` names
   `dbms.ssl.policy.bolt.tls_versions=TLSv1.2,TLSv1.3`, so a stock Neo4j offers TLS 1.3 whatever its default (Neo4j 5
   and 2025 have the setting; their JVMs speak TLS 1.3). A Neo4j that cannot is the key-file case.
4. *Later, if wanted: a tunnel in the pod.* For a driver that can only read key files (stock neo4rs, or another
   client in another language), a small TLS client in the daemon's image: the workload connects to it in plain text
   on the pod's loopback, and it connects to Neo4j with the daemon's key. No key file, any driver. The cost: loopback
   has no peer check, so any container in the pod can use it, where the daemon's socket admits only the pod's own
   UIDs. Not built now.
*Done (2026-10-27).* Item 1 holds as built in steps 2 and 3, and now for the graph too. Item 2 is built with step 4,
since until a chart has the sidecar every target renders as "off". Item 3: the Bolt policy of `local`,
`local-minimal` and `sandbox-local` names `TLSv1.2,TLSv1.3`. The render diff is that line in Neo4j's configmap
alone, and the setting is one Neo4j 2025.08 declares (`SslPolicyConfig`: `dbms.ssl.policy`, the Bolt scope,
`tls_versions`), since Neo4j refuses to start on a key it does not know. Item 4 is not built.

5. *Not degraded, not planned:* a second, Neo4j-only client certificate with its own key file. It is one client
   certificate per pod (`infra/config/capabilities.nuon`), and a second for the same identity and type supersedes the
   first (`deployments.md`, "The renewal rule").

*Built (2026-10-27): the Polar side of the neo4rs patch.*
- *The pin:* the workspace's neo4rs is the fork at `4117f5a`, with the comment.
- *The graph's key:* `GraphTls::Mutual`'s key is `ClientKey` (`lib/src/graph/environment.rs`): `GRAPH_CLIENT_KEY`, or
  `POLAR_CUSTODY_SOCKET` read through the Cassini client's declaration, exactly one when `GRAPH_CLIENT_CERT` is set.
  Without a client certificate the socket says nothing to the graph.
- *The controller* (`lib/src/graph/controller.rs`, `neo_config_for`, `custody_tls`) builds the daemon's configuration
  with `polar_custody::client::client_config` and hands it to neo4rs with `with_rustls_config`. A key file goes as
  before.
- *Tests:*
  - the custody key, and both refused;
  - the socket without a client certificate;
  - every combination of the four variables, the section judging each as the controller reads it;
  - end to end: neo4rs, configured for the daemon, connects to a TLS 1.3 stand-in that demands a client certificate
    under the CA, which sees the daemon's certificate.
- *Mutations caught:* no client certificate presented; both keys accepted.
- *Not yet seen against a real Neo4j:* step 7.

## Step 4, the plan (2026-10-27, its question decided)

What is there, read for this:
- *How a pod is made.* A workload's kind (`infra/config/kinds.nuon`: `agent`, `service`, `ca`) and its capabilities
  (`capabilities.nuon`) are resolved (`infra/resolve.nu`) into a record. `schema/agent.dhall` renders that record as a
  Job: polar-init the one init container (`functions.makePolarInitContainer`), the workload the one container.
- *Every container is UID 1000* (`Constants.DropAllCapSecurityContext`: `runAsUser` and `runAsGroup` 1000), polar-init
  and the workload alike.
- *The escrow key needs that.* polar-init loads it into the user keyring of its UID, with permissions for possessor and
  user only (`polar-kernel-keyring/src/lib.rs`, `keyctl_setperm(serial, KEY_POS_ALL | KEY_USR_ALL)`). The workload
  adopts it as the same UID. A polar-init under another UID would put the key where the workload cannot reach it.
- *The key variables:*
  - `TLS_CLIENT_KEY` (the `cassini` capability, and `Constants.commonClientEnv`, which polar-init is given too);
  - `GRAPH_CLIENT_KEY` (`graph-write`, the same file);
  - `TLS_SERVER_KEY` (`cassini-server`, the broker's listener, `cassini/broker/src/listener.rs`, `with_single_cert`);
  - `POLAR_GIT_SERVER_KEY` (`git-server`, its listener, `polar-git-server/server/src/tls.rs`).

  Each certificate is one `--cert` built in `resolve.nu` (`$"($cert_kind):($c.dir):ecdsa-p256:($san)"`). The git
  server holds a client and a server certificate; every other workload holds one.
- *The schema can say it.* dhall-kubernetes 1.31 (`schema/kubernetes.dhall`) renders a container's
  `restartPolicy: Always`, which makes it a native sidecar, so step 4 does not wait for 1.35.
- *Images are Nix attributes* (`infra/images.nuon`: `polarInitImage` and the rest).
- *No pod carries a seccomp profile yet*, so the design's "under the builder's seccomp profile" waits for the runtime
  attestation work.
- *Not the daemon's to hold:*
  - cert-issuer, the `ca` kind with no polar-init, which makes its own key and refuses the socket (step 2);
  - Neo4j's server, whose JVM reads files;
  - the `ace` job's Neo4j client, a hand chart.

  They keep key files, so step 7's "no `key.pem` in any pod" reads "in any pod but these three".

The plan:
1. *The two roles, one UID: each slot sealed once installed* (decided, below). The daemon tells polar-init (who makes CSRs and installs) from
   the workload (who signs) by UID, and here they must be the same UID.
2. *The daemon's image and its container.*
   - `polarPkgs.polarCustody.image` beside `polarInitImage`, listed in `images.nuon`.
   - The container is a native sidecar: an init container with `restartPolicy: Always`, declared before polar-init,
     so it starts first and lives as long as the pod.
   - It runs under its own UID, 1001, group 1000, so it shares the socket with the pod's containers. Its socket is
     0660 and checked by `SO_PEERCRED`. It runs non-root with every capability dropped but `IPC_LOCK`, so that
     `mlockall` can lock its memory, as the design has it; without the capability a container's memory-lock limit
     makes the lock fail, and that is only logged.
   - Its environment:
     - `POLAR_CUSTODY_SOCKET=/run/polar-custody/custody.sock`;
     - `POLAR_CUSTODY_CA_CERT=/etc/polar-ca/ca.pem` (the `pinned-ca` capability's mount, which every `agent` and
       `service` has);
     - the installer's and the workload's UIDs.
   - A memory-backed `emptyDir`, `polar-custody`, at `/run/polar-custody` in the sidecar, polar-init and the workload.
3. *What the catalog renders for a pod with the daemon* (resolved in `resolve.nu`, rendered in `agent.dhall`):
   - every certificate polar-init issues is `type:dir:custody:sans` (one per type: a render error otherwise);
   - polar-init is given `POLAR_CUSTODY_SOCKET` and not `TLS_CLIENT_KEY`;
   - the workload is given `POLAR_CUSTODY_SOCKET`, and not the key variables. Each capability marks its key variable
     (`key_of: "client"` or `"server"`), and a marked variable is left out under the daemon.
4. *The switch* (degraded operation, item 2):
   - a target's `custody` setting, on by default;
   - a workload's `custody: false` to leave one out;
   - the `ca` kind never has the daemon.

   Off renders exactly what renders today; the test is the render being byte for byte the same.
5. *The two listeners take the daemon's server key.*
   - `polar_custody::server`-side, beside `client_config`: a rustls server configuration offering TLS 1.3 alone, its
     certificate resolved to the daemon's server slot.
   - The broker reads `TLS_SERVER_KEY` or the socket, exactly one, as the client does; so does the git server
     (`POLAR_GIT_SERVER_KEY`).
   - Every client of either is rustls and speaks TLS 1.3.
6. *A daemon that restarts: the agent's health actor decides* (the maintainer, 2026-10-27).
   - A sidecar that fails is restarted by the kubelet, empty: no key and no certificate, and nobody installs one, since
     polar-init has run. Sessions already up go on; no new handshake can be made, ever.
   - The agent's health actor is the authority on whether the agent goes on, not the Kubernetes probe. The probe is
     the fallback for a domain supervisor that has stopped answering, when the pod must be killed. This is not that:
     the supervisor answers, and the agent can end in order.
   - So `HealthCheckActor` (`polar-health`) watches the daemon. Each tick, with `POLAR_CUSTODY_SOCKET` set, it asks
     for the slot's certificate and compares it with the one in its certificate path. An empty daemon, or a different
     certificate, enters the rejuvenation path at once (decided: `Rejuvenate`, not `Unhealthy`): it waits until the
     dependency certificates are healthy, so the replacement can be issued, jitters, and the agent ends in order,
     serving on its existing sessions meanwhile.
   - A daemon that cannot be reached is tolerated for a few ticks, as Cassini is ("3 times in a row"), then the same.
   - The pod is replaced: by the Job now, in place after the move to Deployments. The renewal rule admits the new
     certificate.
   - `polar-healthcheck`, the probe, is unchanged: it reads the health file's freshness.
7. *Tests first:*
   - the render with the switch off, byte for byte the same as before;
   - with it on:
     - the sidecar first, with its UID, group and capability;
     - the socket volume in the three containers;
     - every `--cert` `custody`;
     - no key variable in polar-init or the workload, and the socket in both;
     - the `ca` kind and `custody: false` workloads unchanged;
   - two certificates of one type under the daemon refused at render;
   - the broker and the git server: a TLS 1.3 handshake whose server signature the daemon made, with `TLS_SERVER_KEY`
     or the socket, exactly one;
   - the health actor: rejuvenating on an empty daemon and on a different certificate, tolerating an unreachable one
     for its ticks and then rejuvenating, and nothing while the daemon holds the certificate;
   - `infra/tests/rules.nu` and `charts.nu` brought along.
8. *After it:* the speed gate (step 6, closed 2026-10-27), then a fresh sandbox-local (step 7). Revocation (step 5) can come before or
   after; the renewal rule is in.

*Built (2026-10-27): the sealing.* `Custody::handle` (`polar-custody/src/custody.rs`) refuses `MakeCsr` and `Install`
on a slot holding a certificate, with `Refusal::Sealed`; the role check comes first, so a caller without the installer
role is still `NotPermitted`. A refused certificate does not seal. Tests: an installed slot sealed and the other not, a
refused certificate not sealing, and a property, that once installed nothing any caller asks changes the slot's key or
certificate. The test that a new key drops the old certificate goes with the rule. The mutation that drops the seal
is caught by both.

*Built (2026-10-27): the catalog and the switch* (degraded operation, item 2).
- *The switch:* `cluster.nuon` `custody: { enabled: true }`, a target's `cluster` overriding it; the kind's and entry's
  `custody`, true for `agent` and `service`, false for `ca`.
- *The resolver* (`infra/resolve.nu`, `custody_of`):
  - custody `--cert`s;
  - the key variables left out (`key_of` in `capabilities.nuon`);
  - the socket's volume in the three containers, and `POLAR_CUSTODY_SOCKET` in the document and in polar-init;
  - two certificates of one type refused (`rules.nu`, `custody_cert_problems`);
  - the record's `custody`;
  - the daemon's image listed by `resolve.nu images`.
- *The template* (`schema/agent.dhall`, `agent-spec.dhall`): the daemon as a native sidecar before polar-init, UID
  1001, group 1000, `IPC_LOCK` alone, with the socket and the pinned CA mounted; polar-init with
  `Constants.custodyClientEnv`, no `TLS_CLIENT_KEY` (`functions.makePolarInitContainerWith`).
- *Off is as before:* with the switch off, `sandbox-local`, `local` and `local-minimal` render byte for byte what they
  rendered before the change, checked against a render taken first. On, all three render.
- *Found on the way:*
  - *The scheduler observer clones with the `git` command,* whose libcurl reads `POLAR_SCHEDULER_GIT_CLIENT_KEY` from
    a file, so its entry says `custody: false` until an in-pod tunnel signs for it ("Degraded operation", item 4). It
    is the one pod of the catalog on key files. `polar-sign`, which also runs `git`, is run by people.
  - *The images step would have refused the render.* `up.nu` prepares the images `resolve.nu images` lists and refuses
    a render naming another, and the list did not have the daemon's: it does now.
  - *`render-catalog.nu --only`* takes a comma-separated string. A list cannot be given to a script from a shell, as
    `remaining-work.md` had recorded.
  - *`infra/tests/resolve.nu` takes a scaffold directory already given,* so it can run without a cluster, from the
    workspace's own binaries' `--scaffold`. The four tests that run the agents' own `--check` still need one.
- *Tests (`infra/tests/resolve.nu`, `rules.nu`):*
  - a pod's certificates, variables and volumes under the daemon;
  - the rendered pod: the sidecar first, its user, group and capability, the socket shared, and polar-init without the
    key;
  - the broker's and the git server's keys;
  - cert-issuer and the scheduler observer without the daemon;
  - a target with it off;
  - the daemon's image listed;
  - one certificate of each type.
  
  The broker's and the graph agents' tests now check both modes. 52 of 56 pass offline; the 4 that need a cluster are
  as before.
- *Mutations caught:* the key variables kept, the certificates left `ecdsa-p256`, the sidecar not a sidecar, the
  daemon's image unlisted.

*Built (2026-10-27): the image.* `src/agents/polar-custody/package.nix` and `container.nix`, as `polar-init`'s:
- the daemon alone, without a CA bundle or OpenSSL, since the CA it trusts is the pod's, mounted;
- built in UID 1001, group 1000; the pod's security context sets them too;
- `polarCustodyImage` in the flake, `polar-custody` in `infra/images.nuon`.

It builds (`nix build .#packages.x86_64-linux.polarCustodyImage`, `polar-custody:latest`). The binary in it, run with
no environment, names its four variables and exits 2. `infra/tests/images.nu` passes.

*Built (2026-10-27): the health actor watches the daemon* (`polar-health`).
- *The reading* (`src/custody.rs`): each tick, with `POLAR_CUSTODY_SOCKET` set (`HealthConfig::custody_socket`), the
  actor asks the daemon for each slot's certificate, off its own thread: it `Holds` one, is `Empty`, or is
  `Unreachable`. It does not compare the certificate with the one on disk, as item 6 first said. A slot once installed
  is sealed, a daemon that restarts loses both, and polar-init installs before it writes `cert.pem`, so a
  different certificate cannot be.
- *The rule* (`machine.rs`, `custody_rejuvenation`), in the Normal phase and before the connections, since an empty
  daemon may be why they are down:
  - `Empty` enters rejuvenation at once;
  - `Unreachable` is borne for two ticks and enters it on the third (`MAX_CUSTODY_UNREACHABLE_TICKS`), the count
    starting again when the daemon answers.
- *The hard deadlines still come first:* Cassini down past its deadline while the agent waits to rejuvenate is that
  deadline's `Unhealthy`, an orderly end all the same.
- *Tests:*
  - the reading against a real daemon: empty, holding a certificate in the server slot alone, and none at all;
  - the machine: nothing while the daemon holds its certificate; rejuvenation at once on an empty daemon, Cassini
    down; Cassini's deadline coming first; the unreachable count;
  - the actor reaching `Rejuvenate` on an empty daemon though its certificate is far from rejuvenation;
  - the config reading the socket.
- *Mutations caught:* the actor never asking, an empty daemon ignored, the unreachable count never reset.

*Built (2026-10-27): the listeners.*
- *`polar_custody::client::server_config`* and `ServerKey`: a server configuration with a key file as before, or the
  daemon's server slot, offering TLS 1.3 alone (`CustodyServerCert` resolves every client to the slot's certificate).
- *One declaration:* `polar_custody::environment` declares `POLAR_CUSTODY_SOCKET` and the rule `one_key`, a key file
  or the socket, exactly one, which the Cassini client (re-exporting it), the broker and the git server use. The agent
  builder refuses a variable declared twice, so the git server reads the socket through the Cassini client's section,
  which it registers; the broker, which is not a Cassini client, declares it in its own.
- *The broker* (`cassini/broker/src/settings.rs`, `read_server_key`; `listener.rs`) and *the git server*
  (`polar-git-server/server/src/settings.rs`, `read_key`; `tls.rs`, which still sets its ALPN) take
  `TLS_SERVER_KEY` / `POLAR_GIT_SERVER_KEY` or the socket.
- *The git server's clients include the git CLI* through libcurl, not rustls alone, as item 5 first said. TLS 1.3 alone
  still holds: current OpenSSL and GnuTLS speak it.
- *Tests:*
  - a server whose key is the daemon's verified by a TLS 1.3 client with the certificate installed;
  - such a server refusing TLS 1.2, by its own reason;
  - a key-file server as before;
  - the one-key rule's every combination;
  - the broker's and the git server's settings;
  - the git server speaking HTTP/1.1 either way;
  - a real Cassini client registering with a broker whose key is the daemon's.
- *Mutations caught:* the daemon's server offering TLS 1.2, a resolver giving no certificate, the broker ignoring the
  daemon's key.
- *Found on the way:* a server that signs in its handshake blocks the Tokio worker it runs on for the round trip. A
  daemon run as a task on the same runtime, as a test first did, can be left waiting behind it, and the handshake times
  out. In a pod the daemon is a process of its own, and the tests now run it on a thread and runtime of its own
  (`daemon_apart`).

*Decided (the maintainer, 2026-10-27): (a), each slot sealed once installed.* The question, as it was put: how the
daemon keeps its installer apart from its workload, under one UID.
- *(a) Seal each slot once installed (recommended).* The daemon takes `MakeCsr` and `Install` for a slot until a
  certificate is installed in it, and then only `Certificate` and `Sign`. The workload starts after every init
  container has finished, so each slot it uses is sealed before it runs. A compromised workload cannot replace the key
  or the certificate; on a slot it does not use it can make a CSR and nothing more, since it has no token to have one
  signed. One change in `custody.rs`, a refusal of its own, and its tests and a property.
- *(b) polar-init under a UID of its own.* The roles are kept as built, but the escrow key's hand-off through the user
  keyring breaks (it is per UID), so the escrow custody changes with it. Larger, and in code that works.
- *(c) Both roles to UID 1000, nothing more.* A compromised workload could replace the key (`MakeCsr`), which is a
  denial of service and no more, since it cannot have a certificate signed. The weakest.

## Step 5, revocation: the plan (2026-10-27, its question decided)

What is there, read for this:
- *What is revoked.* The credentials agent's ledger puts a serial in `revoked_serials`
  (`credentials-agent/consume/src/state.rs`) for an administrator's `Revoke` (the admin CLI's `revoke`) and for every
  certificate superseded (the renewal rule, `deployments.md`). The whole set is published, signed, on
  `polar-authority-updates` (`AuthorityBody::Revoked`). Every builder agent holds it (`RevokedSerials`,
  `polar-agent-builder/src/authority.rs`), and so does the broker, through an `AuthorityHandle` of its own
  (`cassini/broker/src/authority.rs`, `BrokerAuthority`). Today it is consulted only for the signer of a signed
  document (`polar-agent-builder/src/signed.rs`).
- *A serial's text* is the lowercase hex of the certificate's raw serial (`cert_issuer_common::serial::serial_from_cert`),
  the form the ledger keys by. The broker and the daemon read it the same way.
- *The broker admits a revoked certificate.* The listener reads the identity and the floor from the leaf after the
  handshake (`listener.rs`, `extract_peer_identity`, `extract_peer_floor`), not the serial, and nothing consults the
  set at accept or afterwards. A session whose certificate is revoked runs on.
- *The precedent for ending what is no longer admitted.* Every change of authority makes every listener judge its
  subscriptions again (`authority.rs`, `ChangeCounter`, `JudgeSubscriptions`), and one no longer admitted is dropped.
- *One live session per identity.* A new session supersedes every older session of its identity and closes its
  connection (`cassini/broker/src/session.rs`, `supersede_older`). So a daemon that registered as the pod, as "The
  design" has it, would close the workload's connection, and the workload's reconnection would close the daemon's:
  the two would take turns. This is the question below.
- *A crate cycle.* `cassini-client` depends on `polar-custody` (`ClientKey`), so the daemon cannot use the Cassini
  client from inside the `polar-custody` package. A daemon with a session of its own moves to a crate of its own.
- *The broker's pod has no client slot.* It holds a server certificate alone (`cassini-server`), so its daemon has no
  identity to connect as.
- *Lifetimes.* A client certificate lives an hour (`CERT_ISSUER_DEFAULT_LIFETIME_SECS`, 3600), a server certificate
  12 hours on the three local targets (`CERT_ISSUER_SERVER_LIFETIME_SECS`).
- *No other verifier checks revocation.* The git server, cert-issuer and Neo4j trust the CA alone.
- *A revoke does not keep the identity out.* `Revoke` names a serial. The pod's next request is admitted by the
  renewal rule after its five minutes, so revocation ends a key and its sessions, and the credentials agent decides
  whether the pod is issued again (decided item 1), by its rules as they are.

The plan:
1. *The broker refuses and ends a revoked certificate's sessions* (decided item 2; needs no answer below).
   - The listener reads the leaf's serial beside the identity. `RevokedSerials` gains a lookup by serial.
   - At accept, a revoked serial is refused before any listener is made: the connection is closed and logged.
     (Built at registration instead, a watcher admitted: "Changed in building it", below.)
   - On a change of authority, every listener whose serial is now revoked closes its connection and ends its session.
     The session ends as a connection closed without a goodbye, kept for the retention window: revocation is of a
     certificate, not of the identity, so the identity's next pod, under a new certificate, claims what was held, as
     after any restart. The revoked certificate cannot resume it, since it is refused at accept.
2. *The daemon watches for its own serial* (the shape depends on the question below; as recommended):
   - The daemon's binary moves to a crate of its own, `polar-custody/daemon`, the library staying where it is. The
     image is the same.
   - Once its client slot holds a certificate, it connects to Cassini as the pod, its own handshake signed in process
     by the slot's key (a third `ClientKey` kind, made only in code, never from the environment), and registers as a
     *watcher* (below). It subscribes to the two authority topics and asks for a snapshot, and pins the credentials
     agent's key from the first snapshot that agent publishes, by the rules every agent keeps (`AuthorityCore`).
   - When a set it believes holds a slot's serial, it destroys that key (the slot removed, so `SlotKey` zeroes it),
     logs it, drops every slot, and exits non-zero (decided item 1). The kubelet restarts the sidecar empty; the
     health actor sees an empty daemon and rejuvenates (step 4, item 6); the pod is replaced and asks again.
   - It signs while it has no authority yet, and while Cassini is away. Otherwise the credentials agent's own pod could
     not start, its daemon waiting on the credentials agent. A revocation published while its session is down is seen
     in the snapshot it asks for on its return.
   - The broker's daemon does not watch (no client slot): the broker's server key ends with its certificate, 12 hours
     at most. A limit, recorded.
3. *A watcher session, in the broker* (as recommended below): declared at registration (a flag on the wire's
   `RegistrationRequest`). It never supersedes and is never superseded, nothing is held for it (its close is a
   goodbye), and it may subscribe to the two authority topics and publish snapshot requests, nothing else, whatever
   its certificate's floor or policy say. It is self-declared, and harmless so: a watcher can do less than the
   identity's own session, so declaring one gains a compromised workload nothing.
4. *Tests first:*
   - the broker: a revoked certificate refused at accept; a connected session closed and ended when its serial is
     revoked, another identity's untouched; a property, that after any sequence of revoked sets and connections no
     open connection's serial is in the set the broker holds;
   - the watcher: beside its identity's session, neither closes the other; refused outside the authority topics;
     nothing held after it goes;
   - the daemon: its serial revoked, its key destroyed, nothing signed after, and it exits; another serial, nothing; a
     set signed by another key, or published by another identity, nothing; a property, that for any sequence of sets
     it signs exactly while no set it believed has held its serial;
   - end to end, in the broker's tests: a broker, a daemon, a workload's session and a credentials agent stand-in
     publishing signed sets; the serial revoked, the workload's session ended and the daemon's key gone.
   - Mutations on every rule.
5. *Docs:* `signed.rs` says "nothing revokes yet" twice, stale since the ledger revokes; the comments follow.

*The question: how does the daemon hear of its revocation?*
- *(a) A watcher session of its own,* as above. The daemon hears it whatever the workload does. The cost: the broker's
  session rules gain a kind, the wire a flag, and the daemon a Cassini client and a crate of its own.
- *(b) The workload relays it.* The workload's agent already holds the signed sets and hands each to the daemon over
  the socket, which verifies the credentials agent's signature itself. No change to the broker's sessions. But a
  compromised workload withholds its own revocation, which is the case revocation is for, and the key the daemon
  pins comes through the workload too.
- *(c) The broker alone.* Only item 1. A revoked key goes on signing until its certificate ends (an hour), useful
  against the verifiers that do not check revocation (Neo4j, the git server, cert-issuer).

Recommended: (a).

*Decided (the maintainer, 2026-10-27): (a), a watcher session of the daemon's own.*
What a watcher is, its rules and its weak points: `cassini-broker.md`, "Watcher sessions"; how the daemon's
snapshots work: `authority-distribution.md`, "Snapshots".

*Changed in building it (2026-10-27): refused at registration, not at accept.* Item 1 as planned closed every
connection under a revoked certificate. But the daemon's watcher presents the pod's certificate, so the broker would
close it too, perhaps before the update reached it, and refuse it ever after, and the daemon could then never hear of
its own revocation. So a revoked certificate is refused at registration unless it registers a watcher, and on a change
of authority every connection whose certificate is now revoked is closed, a watcher's apart. A watcher that comes back
asks for a snapshot, which carries the set. The cost is that a revoked certificate's holder can still hear the
authority topics as a watcher until the certificate ends, which every agent hears anyway; with the daemon nobody holds
the key once it is revoked.

*Built (2026-10-27): the broker's part* (items 1 and 3).
- *The wire:* `ClientMessage::RegistrationRequest` and `BrokerMessage::RegistrationRequest` carry `watcher`;
  `TCPClientConfig::as_watcher` asks for one, and a watcher's client never asks to resume
  (`cassini/client/src/lib.rs`).
- *The listener* (`cassini/broker/src/listener.rs`): the leaf's serial read at accept (`extract_peer_serial`,
  `cert_issuer_common::serial::serial_from_cert`); a revoked certificate refused at registration
  (`authority::REVOKED_REFUSAL`) and closed, unless it registers a watcher; on each change of authority
  (`JudgeSubscriptions`) a revoked agent's connection closed, its session kept for the retention window; a watcher held
  to the three authority uses (`authority::watcher_may`, `WATCHER_REFUSAL`), and within them decided as its identity's
  session is.
- *Sessions* (`cassini/broker/src/session.rs`): a watcher neither supersedes nor is superseded, is never resumed nor
  resumes another, ends when its connection goes, and claims nothing held for its identity (`GetHeldSessions`).
- *`RevokedSerials::contains`* (`polar-agent-builder/src/authority.rs`).
- *Found, and fixed, on the way:* a superseded session with no subscriptions was held at once and stopped
  (`complete_takeover`) before it had closed its connection: a stop is a signal, ahead of the queued
  `CloseConnection`. The old instance stayed connected with no session about one time in five. It is drained now.
  Also found, not fixed: the control plane's `ListSessions` always answers `InternalError`, since the session manager
  has no arm for `GetSessions` (`remaining-work.md`).
- *Tests:* `tests/revocation.rs` (a session ended on revocation and another identity's not; a revoked certificate
  refused registration and admitted as a watcher; a watcher left to hear its own revocation; the identity's next
  certificate claiming what the revoked one's session held; a property over clusters of agents and watchers, any of
  them revoked, eight cases, each a broker of its own); `tests/watcher.rs` (side by side with its identity's session,
  either first; nothing but authority; its session ended when it goes, and nothing held claimed by it);
  `tests/supersede.rs` (the race above, six rounds); the wire's round trip of the flag
  (`cassini/types/tests/props/lib.rs`). Mutations caught: each of the listener's and the session manager's rules
  dropped or widened, and the drain put back to a stop.

*Built (2026-10-27): the daemon's part* (item 2).
- *Its own crate:* the binary moved to `polar-custody/daemon` (package `polar-custody-daemon`, binary still
  `polar-custody`, so the image's build is unchanged), since `cassini-client` depends on `polar-custody`. `libc` and
  `tracing-subscriber` went with it.
- *The custody* (`polar-custody/src/custody.rs`): `Custody::revoke(serials)` destroys every key when a set names a
  certificate it holds (the slots go, each key zeroed as it goes), and from then on every request is refused with
  `Refusal::Revoked`; `serial` and `chain` read a slot's certificate.
- *Its own handshake* (`polar-custody/src/own.rs`): `own_client_config`, the client slot's certificate signed for in
  process through the same rule as every signature (`OwnSigningKey`, the workload's role), TLS 1.3 alone. The plan
  had a third `ClientKey` kind; the Cassini client takes a TLS configuration made in code instead
  (`TCPClientConfig::with_tls`, the files then never read), as neo4rs takes one (`with_rustls_config`), which leaves the
  three places that match on `ClientKey` alone.
- *The authority:* `AuthorityCore`, the decision with no actors, moved from `polar-agent-builder` to
  `credentials_agent_common::authority_leaf`, with its tests, and the builder re-exports it, so the daemon keeps the
  credentials agent's authority by the rules every agent keeps without the builder's policy engine in the process
  that holds the keys.
- *The watch* (`polar-custody/daemon/src/watch.rs`): `Watch` says what something heard means for the keys held, and
  revokes in the custody; the `Watcher` actor connects, once the client slot holds a certificate, as a watcher session,
  subscribes to the two authority topics and asks for a snapshot each time it registers, reconnects on each tick after
  a lost connection, and on a revocation stops its connection and calls `on_revoked`. The binary exits then with code 3
  (`REVOKED`); the kubelet restarts the sidecar empty.
- *Its environment* (`main.rs`): `BROKER_ADDR` and `CASSINI_SERVER_NAME` now required, and `POLAR_AUTHORITY_PUBLISHER`
  with every agent's default (a test holds the two defaults together). The catalog gives the sidecar the broker every
  client in the pod is given (`infra/schema/agent.dhall`, `Constants.cassiniAddr`, `cassiniDNSName`). The broker's own
  pod has no client slot, so its daemon never connects (the limit recorded above).
- *Tests:* the custody's revocation (its serial, another, a server slot's, a key without a certificate) and a property
  that it signs exactly until a set names it; the daemon's own handshake (TLS 1.3 alone, by the server's reason; failing
  once revoked; none without a certificate); the client's configuration made in code used without reading a file;
  `Watch` (a snapshot or a later update naming it, other serials, an impostor, another key, an update before a key) and
  a property over sequences of what is heard, that it signs exactly until a set it believes names it; end to end
  (`cassini/broker/tests/custody_revocation.rs`): a revoked pod's daemon told and its keys gone, its workload's
  session ended; a revocation published while the daemon was away heard in the snapshot it asks for; signing before
  any authority, as the credentials agent's own pod must; the workload and the watcher side by side for three seconds
  with no supersession either way; and the watcher back after Cassini restarts. The catalog: the sidecar given the
  pod's broker (`infra/tests/resolve.nu`). Mutations caught: each rule of the custody's revocation, the watcher's
  subscribing, asking, being a watcher and reconnecting, `Watch` revoking and asking, the own configuration offering
  TLS 1.2, and the client ignoring a configuration given.
- *Not run:* the four resolve tests that ask the agents' images to judge their documents, which need the cluster's
  registry; the cluster was down. They do not cover the daemon's environment.
- *Seen on a cluster:* not yet; step 7.

## Step 6, the speed gate: the plan (2026-10-27, closed)

What is there, read for this:
- *Resumption, the question "Speed" left open, answered.* A client reconnecting with the configuration it connected with
  resumes (TLS 1.3, a PSK): no signature on either side. This was checked with the daemon's own configuration, one full
  handshake then two resumed (a scratch test against `server_config`, as the broker builds it). The Cassini client keeps
  one configuration for its life (`cassini/client/src/lib.rs:1027`), and the broker keeps sessions in memory (rustls's
  `ServerSessionMemoryCache`, 256 of them by default). So an agent's reconnects to a running broker cost no signature.
  The storm is the case resumption cannot help: a broker that restarted, its cache empty, and new pods, new processes.
  Past 256 clients the broker's cache evicts, which matters for a large cluster and not for ours.
- *The harness* (`infra/load-harness.nu`, `src/agents/cassini/test`) measures messages: producers and sinks over one
  broker, at sizes and rates (`broker-baseline.md`). It has no storm of connections.
- *The first measurement is not like for like.* Step 1's bench set eight callers through the socket (p99 145 to 244 µs)
  against one caller in process (p99 14 to 25 µs). Eight in-process signers at once would contend too.

The plan:
1. *The criteria, like for like.*
   - *A signature:* at each concurrency of the storm (1, 8, 32 and 128 callers at once), the p99 of a signature through
     the daemon less the p99 of one signed in process, at the same concurrency, is no more than 100 µs.
   - *A broker:* taking C new connections at once (1, 16, 64, 256), each a full handshake with both sides signing, the
     broker with the daemon's key, its clients with theirs, completes them at no less than 90% of the rate it does with
     key files on both sides.
   - Both are confirmed or revised by the first measurement, as "Speed" said, and a build that misses them does not
     ship.
2. *The storm bench, now, without a cluster.*
   - `polar-custody/tests/bench.rs` gains the concurrent signature: in process and through the daemon, at each
     concurrency.
   - `cassini/broker/tests/storm.rs` is new: a real broker (`TestBroker`, its server key a file or the daemon's, the
     daemon on its own thread and runtime), and C clients connecting at once, each a fresh configuration so that
     nothing resumes, their keys files or a daemon's. It reports handshakes a second and the p50 and p99 time to a
     finished handshake.
   - Both are ignored by default, as `bench.rs` is, and run in release. The figures go in this document with the
     machine and the commit.
3. *The storm in the cluster, with step 7.*
   - The harness producer gains a storm mode: C connections at once, for some rounds, each a fresh configuration, each
     registering and saying goodbye, the time to a finished handshake and to registration recorded. A plan,
     `infra/load-plans/storm.nuon`, sets C and the rounds, and a steady rate of reconnects.
   - It runs on the fresh `sandbox-local` with the daemon, then with the target's custody off and the catalog
     redeployed. The records are kept as the baseline's are, and the figures go in `broker-baseline.md`.
4. *If a criterion is missed:* a finding, with what the figures point at, before any change. Where to look first: a
   task spawned for each request in the daemon; its lock; the client's eight idle connections; the broker signing on
   its Tokio workers.

*Built (2026-10-27): the storm bench, and its first figures* (precisionws, i9-13950HX, 24 threads; release builds;
`polar-custody/tests/bench.rs`, `the_daemon_s_cost_at_each_concurrency_of_the_storm`, and
`cassini/broker/tests/storm.rs`; both ignored by default, each asserting its criterion after printing its table, as
built at `a39c43a4`).

- *The first criterion is missed, as it was put.* Added at p99 over in-process signing at the same concurrency, over
  four runs:
  - 1 caller: 0 to 21 µs;
  - 8 callers: 154 to 275 µs;
  - 32 callers: 216 to 305 µs;
  - 128 callers: 1.1 to 7.5 ms.

  The daemon's rate rises the while, to about 320,000 signatures a second at 32 and 128 callers.
- *What the figures point at.* The bench is closed-loop: each caller asks again the moment it is answered, so it holds
  the daemon at its capacity, and the time is queueing. At 8 callers and 165,000 a second, Little's law gives 48 µs a
  signature, and the median measured is 45 µs. In process the same threads sign in parallel on 24 cores with no queue
  between them, so "the same concurrency" is not the same load. Two checks, put back afterwards:
  - *The client's pool* of eight idle connections raised to 256: much the same (190 µs at 8 callers, 1.4 ms at 128),
    so it is not the cause.
  - *The daemon on one thread* in place of a worker for each core: worse (capacity about 55,000 a second, 780 µs at
    32), so its multithreaded runtime is what gives the capacity, and the hops between threads cost less than they
    give.
- *The second criterion holds, within the bench's noise.* The broker's rate with the daemon's key, as a share of its
  rate with a key file:
  - 1 at once: 102 to 172%;
  - 16 at once: 80 to 101%, two rounds of about 1.5 ms where one slow round moves the median;
  - 64 at once: 100 to 111%.

  The p99 of a handshake is no worse with the daemon (3.2 to 3.4 ms at 64, against 7 to 31 ms with key files).
- *At 256 at once both break alike,* the daemon nothing to do with it: a p99 of 1.0 to 1.1 s, a TCP connection retried
  after its SYN went unanswered, and some rounds at 250 a second, with the host's `somaxconn` and SYN backlog at 4,096.
  The broker's accepting at that rate is a finding of its own, apart from custody.

*Decided (the maintainer, 2026-10-27): (a), the storm bench is the gate of record,* run in release before a custody
release; the cluster's storm at step 7 confirms it under real pods and cgroups. The question, as it was put: (a) the
bench now, the cluster confirming; (b) the cluster's storm alone, the gate waiting for step 7; (c) the bench alone.

*Closed (the maintainer, 2026-10-27): no further measurement, and no gate.* The first criterion measured the wrong
thing. The daemon signs once for each full handshake, so once for a connection, never for a packet or a byte, and a
resumed connection costs no signature at all; what it adds is paid at connecting and nowhere else. Its capacity, about
320,000 signatures a second, is some six times Cassini's ceiling of 40,000 to 50,000 requests a second (the
maintainer's figure), even were every request a new connection, and the broker's handshake rate with the daemon's key
holds against key files at every concurrency the bench could reach. So:
- both benches stay, as measures that print their tables and assert nothing
  (`the_daemon_s_cost_at_each_concurrency_of_a_storm`, and
  `a_broker_s_handshake_rate_in_a_storm_with_a_key_file_and_with_the_daemon`), ignored by default and run in release
  when a figure is wanted;
- the cluster's storm (item 3) is not built; step 7 runs without it;
- the broker breaking at 256 connections at once is carried as a finding of its own (`remaining-work.md`), apart from
  custody.

## Step 7, a fresh sandbox-local: the plan (2026-10-27, cleared and done)

The first time any of steps 2 to 5 runs on a cluster. It goes onto nix-usernetes' multi-node `u7c` (branch
`feat/multi-node`, commit `feaa20a`), which needs a real Polar bring-up before it merges, and the report goes back to
that session.

1. *The cluster.* In `~/Documents/projects/nix-usernetes`, inside `nix develop`: the `u7c` images
   (`CP_IMAGE=localhost/nix-usernetes-node:dev`, `WORKER_IMAGE=localhost/nix-usernetes-worker:dev`), then
   `just cluster-reset`, `cluster-up`, `cluster-init`, `cluster-kubeconfig`; Polar then with
   `KUBECONFIG=~/Documents/projects/nix-usernetes/kubeconfig-u7c`. Its apiserver is on host port 7443, so `u7s` is left
   alone, and the `:1.37.1` node image tag is not moved.
2. *Polar.* `nu infra/up.nu sandbox-local --dev`, the tree clean and left alone until it has passed its publish step.
   Mullvad: asked before it is disconnected for the run, and reconnected after. The browser import of the user's
   certificate is the maintainer's, asked for when a test needs it.
3. *What is seen, each recorded with the command that shows it:*
   - *No key file:* no `key.pem` in any pod but cert-issuer's, Neo4j's and the `ace` job's, and every daemon holding
     its pod's certificate (the health actor's watch quiet);
   - *the listeners:* every agent registered with a broker whose key is the daemon's, and a push to the git server,
     whose key is too;
   - *Neo4j:* a graph writer's queries over a TLS 1.3 handshake its daemon signed, the first against a real Neo4j
     (neo4rs, the fork);
   - *the scheduler observer* on key files, as its entry says;
   - *each daemon's watcher* registered beside its workload, and the broker's daemon not connecting;
   - *revocation:* one agent's client certificate revoked with the admin tool (`revoke <serial>`), kube-observer's
     unless something says otherwise; then its workload's session closed by the broker, its daemon logging the
     revocation and exiting 3, the sidecar restarted empty, the health actor rejuvenating, the Job's new pod issued a
     new certificate and registered, and the old serial refused at registration.
4. *If something fails:* a finding, with what the logs point at, before any change; and nothing edited while `up.nu`
   runs.
5. *The report to nix-usernetes:* whether `up.nu` brought Polar up on `u7c`, what differed from `u7s`, and what changed in
   Polar. Known before starting: Polar's image eviction (`infra/images.nu`) knows only the `u7s` container, so an image
   rebuilt during the run would stay stale on `u7c`'s worker nodes. A fresh cluster has no cache, so it matters only if
   something is rebuilt mid-run; the fix is Polar's to decide, and is reported, not made.
6. *Not in it:* the Cassini load harness with the daemons (`remaining-work.md`, later), a run with custody off (the
   render tests show it byte for byte), the pinning and the watcher cap (planned, to do).

*Cleared (the maintainer, 2026-10-27), kube-observer the one revoked; Mullvad disconnected for the run; the browser
step not waited at.* *Done (2026-10-27), on `u7c` (nodes `cp` and `core`), from `d0df015c`:*
- *The bring-up:* `up.nu sandbox-local --dev` brought all six layers Ready, signed and published the policy, and stopped
  at the browser step, as asked. Its first run stopped at step 8 (below, finding 1).
- *No key file:* no mTLS key in any pod with the daemon (the key variables in every pod's spec, and `ls` of every
  directory mounted in every running container). What remains is what was meant to: cert-issuer's, Neo4j's server key
  (its polar-init), the `ace` job's, the scheduler observer's (`custody: false`); and two the plan did not list, the
  deployment git server's `/pki/server.key` (`polar-deploy`, `up.nu`'s own, not a catalog workload) and the credentials
  agent's escrow key on its volume (not an mTLS key: "Credentials agent key custody", `remaining-work.md`).
- *Every pod with the daemon 2/2,* no daemon restarted; every agent registered with the broker, whose key is its
  daemon's; `up.nu` pushed the policy to the git server, whose key is too.
- *Neo4j:* the three graph writers configure Neo4j's mTLS with "its key the custody daemon's", and kube-consumer writes
  (210 `PodContainerState` nodes and the rest, read back through Neo4j's HTTPS API): TLS 1.3 against a real Neo4j
  2025.08, through the neo4rs fork, for the first time.
- *Watchers* registered beside their workloads (the broker's "a watcher went" as pods ended); every supersession was a
  pod's replacement (four jira-observer pods failing on the target's placeholder Jira, one of the git server's below).
- *Revocation,* kube-observer's client certificate (`3159b578…`), revoked with the admin tool at 21:20:26:
  - 21:20:26.5778, the broker applies the new set (version 8); .5790, it closes kube-observer's connection, naming the
    serial; .5802, the daemon destroys its key (`slots=[Client]`); .5803, it logs "revoked: exiting" and exits 3;
    .5838, the broker ends its watcher session;
  - the kubelet restarts the sidecar empty; the workload's reconnects fail at the handshake, which nothing can sign;
  - 21:20:45, the health actor enters rejuvenation ("the custody daemon holds no certificate"); 21:21:15, its
    dependencies healthy, a jitter of 65 s; at the tick of 21:22:45 the Cassini deadline (120 s) is due as well and comes
    first, as a hard deadline does, and the agent shuts down in order (exit 1);
  - 21:23:56, the Job's new pod: polar-init issued a new client certificate (`0519e3dc…`), its key the daemon's, no
    `key.pem`, and the workload registered; its session superseded the revoked one, kept for the retention window, and
    inherited what it held. Three and a half minutes from the revocation to a replacement at work.
  - *Not seen, and cannot be:* the old serial refused at registration. Its key is gone, so nothing can present it.
- *Findings:*
  1. *Stale port-forwards.* `up.nu` takes a forward that is "already running" as good without asking which cluster it
     reaches: four loops from the `u7s` run of 2026-10-26 still pointed at its removed kubeconfig, and the deployment git
     server "did not answer". Stopped by PID, and the run passed.
  2. *The daemon logs nothing at info.* With no `RUST_LOG` its filter is errors only, so "serving" and "watching" never
     appear; the other agents log at info. Its errors appear, so the revocation did.
  3. *No admin can revoke.* `just admin-provision` gives no `c:revoke`, so no account made by the documented flow can
     revoke. For this run an account `revoker` was provisioned (signed by the maintainer's), holding `c:revoke` and the
     one permission the tool requires (`p:u2s:credentials-agent-observer…`).
  4. *A git server supersession at its start* (21:12:02), of a session that had subscriptions: by its look, its
     polar-init's escrow session not having said goodbye in time. Not a watcher, which cannot supersede; not chased.
  5. *jira-observer* fails on the target's placeholder Jira (`example.atlassian.net`), after registering through its
     daemon: the target's, not custody's.
- *For nix-usernetes:* Polar came up on `u7c` with no change to Polar for it. What differed from `u7s`: the nodes are
  `cp` and `core`, not `u7s`; and the image eviction (`infra/images.nu`) knew only the `u7s` container, which did not
  matter on a fresh cluster since nothing was rebuilt mid-run. *Corrected 2026-10-27:* the pods pull Polar's images
  `Always` (`render.nu` checks it), so a cached copy is never what runs, and the gap cost disk only, not stale code as
  this report and the one to nix-usernetes said. It is closed: eviction now reaches every node container of the cluster
  `KUBECONFIG` reaches (`node_containers`).

## The escrow keys into the daemon, the plan (2026-10-27, for review)

Decided item 3. Two keys are left outside a daemon, and they are not alike.

- **An agent's escrow key** (every agent built `.with_encryption()`: today kube-consumer and the secret harness,
  `kubernetes/consume/src/supervisor.rs:444`, `secret-harness/src/supervisor.rs:76`). RSA-4096, a durable identity with a `CertType::Service` certificate, which outlives its
  pods because the credentials agent keeps a copy (escrowed under `agent_escrow_keypair_secret_name`). `polar-init`
  runs the bootstrap (`polar-init/src/main.rs:461`, `ensure_escrow_bootstrapped`, through `CryptoMessage::
  EnsureBootstrapped`): retrieve it (`polar-crypto/src/lib.rs:1057`, the key sealed to an X25519 ephemeral the
  requester makes), or, when nothing is escrowed, generate it, have it issued with the pod's service-account token, and
  escrow it (`:1124`). Then it loads the key into the kernel *user* keyring and leaves the number in the shared volume
  (`polar-init/src/handle.rs`), and the workload's `CryptoActor` adopts it (`:1280`) or, without a number, bootstraps
  and holds the key itself. The key is used for one thing: RSA-OAEP-SHA256 decryption, in three places
  (`handle_decrypt_own_message`, `handle_decrypt_own_escrowed`, `handle_retrieve_secret`). Not on disk, but in the
  user keyring, shared by UID and left behind by a killed pod (`remaining-work.md`, "Key lifecycle in the user keyring"),
  and in polar-init's memory as PEM on the way there.
- **The credentials agent's own escrow key.** The root: RSA-4096, generated by the workload on first start and kept in a
  file on its PVC (`credentials-agent/consume/src/escrow.rs:104`, `load_or_generate`,
  `/var/lib/credentials-agent/escrow-key.pem`), which the workload reads. It signs every authority update (RSA-PSS
  SHA-256 over `DOMAIN` and the payload, `credentials-agent/common/src/authority_update.rs:227`), signs the CSR for its
  own `CertType::Signing` certificate (`AuthorityCertifier`), and decrypts every secret stored with it (OAEP, in
  `retrieve_secret.rs`, `decrypt_on_behalf_of.rs`, `agent_escrow_keypair_bootstrap.rs`). Seven handlers hold it as
  `Arc<EscrowKeypair>`; the authority publisher and certifier clone the `RsaPrivateKey`. A compromised credentials agent
  can copy the file and sign authority (revocations, policy, the pinned key's successor) from anywhere for as long as the
  CA accepts the certificate. This is the last key on disk in a custody pod (`remaining-work.md`, "Credentials agent key
  custody").

### Part A: the agents' escrow keys

1. **A third slot, `Slot::Escrow`, decryption only, of a new key type** (below, "The key type"). In the daemon's locked
   memory: the kernel keyring is not used for it (the maintainer, 2026-10-27), and the kernel loads none of the
   candidate key types in any case. Sealed like the others once it holds a key (`Refusal::Sealed`). A revocation
   (`Custody::revoke`) destroys it with the rest. Its copy in escrow is the credentials agent's, untouched: the next pod
   has it again only if the credentials agent agrees.
2. *Superseded by the custodian (below).* **The bootstrap's key never leaves the daemon.** New requests, all the installer's (`polar-init`):
   - `EscrowExchange`: the daemon makes an X25519 ephemeral and returns its public half. `polar-init` sends it in
     `RetrieveAgentEscrowKeypairRequest`, as `CryptoActor` does now.
   - `EscrowReceive { payload }`: the sealed reply. The daemon opens it with the ephemeral (once: the ephemeral goes,
     whatever the result), checks it is an RSA-4096 private key, and holds it. `polar-init` never has the plaintext.
   - `EscrowCsr { identity }`, for `NotFound`: a new key in the slot and its CSR, which `polar-init` sends for issuance
     with the token, as now.
   - `EscrowSeal`: the slot's new key, sealed for escrow, which `polar-init` stores with `StoreSecret` under the same
     name. **Sealed only to the credentials agent's key the daemon itself pinned** (its watch's `AuthorityCore`, pinned
     under the CA since "Pinning the key to the CA"), never to a key the caller names: otherwise any caller could have
     the key sealed to a key of its own. Once, and only for a key made by `EscrowCsr` and not yet sealed. Without a
     pinned authority yet it is refused (`NoAuthority`), and `polar-init` retries, as it retries the bootstrap now.
2a. *Settled by the custodian (below).* **Found while building (2026-10-27): the workload could still retrieve the key itself.** The credentials agent
   answers a retrieval by the requester's broker identity alone (`credentials-agent/consume/src/
   agent_escrow_keypair_bootstrap.rs`), and seals the key to whatever X25519 key the request names; the workload has
   that identity too. So a compromised workload could publish its own `RetrieveAgentEscrowKeypairRequest` and have its
   escrow key in plaintext, and the daemon would hold it to no end. Two ways to close it:
   - *the service-account token on a retrieval* (recommended): the request carries it, and the credentials agent
     validates it as it does on issuance, for the same identity. The token is readable only in `polar-init`
     (`CryptoMessage::EnsureBootstrapped`'s doc); an agent whose workload mounts its own token (`kube-api-read`) could
     still retrieve, and none with `encryption` does today;
   - *the daemon vouching for its ephemeral*: it signs the X25519 public half with the pod's mTLS key, as nothing a
     caller sends ever is (`Sign` takes a TLS 1.3 CertificateVerify input only), and the credentials agent checks it
     against the requester's certificate. Holds even with a token in the workload, and asks the credentials agent to
     know each requester's current certificate.
   The maintainer took neither: "why ... the daemon cannot be distinguished as its own identity?"
3. **The workload decrypts through the socket.** `Unwrap { wrapped }` since the HPKE choice ("The crypto change"); first written as `Decrypt { slot: Escrow, ciphertext }`, the workload's, returns the
   plaintext (the same oracle the workload holds today with the key). Refused for the other slots. In `CryptoActor`, a
   third holder, `EscrowKeypairHandle::Daemon`, and the three decryption sites become one function over the holder. A
   workload with a daemon socket never bootstraps on its own (it has no token in any case).
4. **What goes, after it is validated on a cluster** ("Clean up after validation"): the user-keyring path, its number
   file and `POLAR_ESCROW_KEY_HANDLE`, `handle.rs`, and `CryptoActor`'s adoption. The in-memory holder stays for tests
   and a workload with no daemon.

### Part A's bootstrap: the custodian, the plan (2026-10-27, cleared by the maintainer)

Replaces items 2 and 2a. The design is the maintainer's (2026-10-27): **the token buys only the custodian's
certificate, and the custodian's certificate buys the rest.**

Why: in items 2 and 2a the daemon could not be told from the workload. They share the pod's certificate, so the
credentials agent could not refuse the workload what it gave the daemon (2a). And the broker could not tell them apart
either, which is why a watcher declares itself (`RegistrationRequest { watcher }`, `authority::watcher_may`) and why a
revoked certificate may still register one ("Watcher sessions", `cassini-broker.md`). Items 2 and 2a also had
`polar-init` drive the escrow bootstrap through socket requests, which the workload could make too, since polar-init
and the workload are one UID ("Step 4", item 1).

**What stays as it is.** Every certificate of a non-bootstrap pod comes through the credentials agent, never from
cert-issuer directly (`cert-issuer-isolation.md`, rolled out 2026-10-14): `polar-init` reaches it through the
observer, the token sealed to the consumer. The renewal rule (`credentials-agent/consume/src/issuance.rs`,
`call_admit`, keyed on the identity, the pod instance and the type, with the five-minute bound of `deployments.md`,
item 2), the grants, the ledger record and cert-issuer's floor all stay where they are.

1. **The custodian's certificate, the only thing the token buys.** At start the daemon makes its own key, in a slot of
   its own, `Slot::Custodian`, that no request ever signs with (`Sign` refuses it): the daemon signs its own
   handshakes in process (`own.rs`), as it does now. `polar-init` asks for the slot's CSR, takes it through the
   observer with the token, exactly as it takes a client certificate today, and installs what comes back; the slot is
   then sealed, like the others. That is all `polar-init` does with the daemon. It has seen the token and a public
   certificate.
   - *A new type, `CertType::Custody`,* as `Signing` was added: client authentication, for the custodian's Cassini
     session, and nothing a TLS server or a secret is for. Its first name is `custody.<X>`, where X is the identity the
     token proves (`custody.kube-consumer.polar.serviceaccount.cluster.local`); cert-issuer derives it from the token,
     and no caller names it.
   - *It says what it may ask for, on X's behalf.* The credentials agent writes it, from the catalog, as `p:` names of
     a new custody kind in the certificate (`cert-issuer/common/src/permission.rs`): a client certificate, a server
     certificate with its names (from the allowlist cert-issuer already keeps per workload), and an escrow key if the
     workload has `encryption`. The daemon reads its own certificate to know what to fetch, and the credentials agent
     checks each request against the same certificate: one source, so they cannot disagree.
   - *Attestation is here.* This is the step where the pod proves itself, so the attestation agent's word, when it
     exists (`runtime-attestation.md`), is asked at this issuance.
2. **The custodian fetches the pod's credentials, over its own Cassini session, as `custody.X`.** None of it is a
   socket request.
   - *X's certificates.* For each slot its certificate names, the daemon makes the slot's key and CSR and asks the
     credentials agent for X's certificate. The credentials agent issues it (through cert-issuer, as it does now) only
     to `custody.X`, only for what the custodian's certificate names, and never past the custodian's own expiry.
   - *X's escrow key.* The daemon asks for it, sealed to an ephemeral it opens itself
     (`ephemeral_exchange::EphemeralSession`); if none is escrowed, it makes a P-256 key, has it issued
     (`CertType::Service`), seals it to the credentials agent's key it pinned under the CA, and stores it under
     `agent_escrow_keypair_secret_name(X)`. The credentials agent answers retrieval, escrow issuance and that store only
     to `custody.X`, and refuses them to X itself: the workload, which is X, can neither retrieve the key nor replace
     it. No token travels on any of these.
   - *Then the daemon says it is ready.* `polar-init` waits on a new `Ready` request, then reads the certificates
     (`Certificate`) and writes the public bundle (the certificates and `ca.pem`) for the workload, as it writes it
     now.
3. **The custodian's authority is spent, not kept** (the maintainer's concern: what went out of scope with `polar-init`
   must not live on in a long-running daemon). The credentials agent answers each kind of request from a custodian
   certificate once: one certificate of each type it names, one escrow retrieval or one issue and store. It accepts
   them only within five minutes of the custodian certificate's issue (the bound `deployments.md` already uses). After
   that every request from it is refused, enforced where the daemon cannot reach. What the daemon keeps is a
   certificate good for listening: the authority updates, its snapshots, and its own revocation.
4. **Forced rejuvenation is untouched.** Nothing renews. The custodian's certificate is issued once per pod instance,
   and none of X's outlives it. X's health actor ends the agent as its shortest-lived certificate nears expiry
   (`polar-health/src/machine.rs`); `RestartAllContainers` restarts the daemon with it (`deployments.md`, "How the
   move is done"), so the new instance has a new daemon with no keys, whose custodian certificate only `polar-init`
   can get, with the token it alone reads. A compromised daemon cannot stretch its life: it has one certificate of each
   type, spent authority, and no token.
5. **What the broker does with it.** The custodian is its own identity, so its session neither supersedes X's nor is
   superseded by it, by the rules every session already has. cert-issuer's floor gives custody identities the
   authority topics and the custody requests, and nothing else; the broker's watcher flag and the registration
   exception for a revoked certificate are no longer needed. The cap of one watcher per serial, with its tenure
   ("Capping watchers"), becomes one session per custodian certificate, by the same rule. When X's certificate is
   revoked, the custodian's session is open and hears it; when the custodian's own certificate is revoked (the pod
   itself), the broker closes its session and refuses it at registration, the rule for every revoked certificate, and
   the daemon, refused as revoked, destroys every key and exits (`Custody::revoke`, which also checks the custodian's
   serial). How the daemon learns it is the refusal's to carry, to be read from the broker when built.
6. **The bootstrap pods** (Cassini and the credentials agent: nothing comes up before them) keep going to cert-issuer
   directly, as now, for their own certificates, which `polar-init` carries into the daemon as today (`MakeCsr`,
   `Install`). They get a custodian certificate the same way, from cert-issuer with the token, for their daemon's
   session; they hold no escrow key, and the spent rule has nothing to spend.
7. **The socket after this.** The installer: the custodian's CSR, its certificate, `Ready` (and, in the bootstrap
   pods, `MakeCsr` and `Install` as now). The workload: `Sign` (client, server), `Certificate`, and `Unwrap` for the
   escrow key (item 3 above). Nothing a caller sends starts a retrieval, an issuance or a seal.

**What the long-running daemon holds after the bootstrap:** X's mTLS keys and its escrow key, which it must hold for
the pod's life to serve them (the escrow key in the user keyring today, reachable by UID and left behind by a killed
pod), and the custodian's key, whose certificate can only listen. What is left is the daemon itself, a long-lived
process the workload can talk to, which `polar-init` never was: kept to a small protocol with sealed slots, and the
hardening already planned (its own UID, not dumpable, locked memory, seccomp).

**Build order, each tests first.**
1. cert-issuer and cert-issuer-common: `CertType::Custody`, its naming from the token, its floor; the custody `p:`
   names, with properties (a round trip, and refusal of a malformed or forged name).
2. The credentials agent: the custodian's issuance on the observer path; X's certificates issued to `custody.X`
   only, within what its certificate names, its window and its expiry, once each; escrow retrieval, issuance and the
   store for `custody.X` only, refused to X. Properties: for any sequence of requests from a custodian, each kind is
   answered at most once, never after the window, and no certificate outlives the custodian's; nothing is answered to
   X that is the custodian's to ask.
3. The daemon: `Slot::Custodian`, its session as `custody.X`, the bootstrap actor (certificates, escrow, `Ready`),
   `Unwrap`, and revocation on the custodian's serial.
4. `polar-init`: the custodian's CSR and install, `Ready`, the bundle from `Certificate`.
5. The broker: custody identities by their floor, the watcher flag still accepted until this is validated.
6. `polar-crypto`: the daemon holder (`Unwrap`); no bootstrap in the workload.
7. The bootstrap pods.
8. On a fresh cluster: no token outside `polar-init`; no key outside a daemon; the workload refused its own escrow key;
   a custodian refused a second certificate and a request after its window; a rejuvenation getting everything new;
   X revoked and the custodian revoked, each ending as it should.
9. After that is validated, the cleanup: the watcher flag and its revoked-registration exception, `polar-init`'s
   escrow bootstrap, the user-keyring path, and the workload's own bootstrap in `CryptoActor`.

**Also in this work** (the maintainer, 2026-10-27: "slice this up and/or fold it in however you'd like"): `CryptoActor`
checks a recipient's escrow certificate before encrypting to its key. Today it takes `public_key_pem` and ignores the
`certificate_pem` beside it (`polar-crypto/src/lib.rs`, `handle_encrypt_for_recipient`), believing the channel. The
certificate must chain to the pinned CA, name the recipient, be a `Service` certificate, and hold that key. Encryption to
another agent stays in the workload: it needs no key the daemon holds, and the custodian's authority is spent.

**Built (2026-10-27), steps 1 to 7, each tests first, mutation-checked:**
1. cert-issuer (`9fde5490`): `CertType::Custody` (client authentication only), named `custody.<X>` from the token on
   `/issue`, vouched for by the credentials agent or, for a bootstrap service, on the token alone with nothing in it;
   the custodians' space reserved (a dotted service account named like a custodian buys nothing; `/internal/issue`
   signs no custodian and no name in the space; a `p:custody:` name only in a custodian's certificate, which carries
   nothing else). `/internal/issue` takes the pod instance and an end (`expires_by`), and checks a server
   certificate's names against the floor. `cert_issuer_common::custody` (the names, `custodian_of`) and
   `identity::kubernetes_subject_of`, the inverse of the normalization, which a grant is looked up by.
2. The credentials agent (`35c7ede2`, `73aeda50`): the custodian's certificate on the token path, recorded under
   `custody.<X>`; `credentials-custodian-issuance-requests` for X's certificates; escrow retrieval, escrow issuance
   (no token now: `AgentEscrowIssuanceRequest` lost its credential) and the store of `agent-escrow-keypair:<X>`
   answered to `custody.X` alone, refused to X; that name never retrieved by name; the request gate admitting a
   custodian on `CUSTODIAN_PUBLISHES`; a bootstrap service's held custodian vouched for as the service is. One pure
   rule, `custodian::judge`, with a property. An escrow certificate's P-256 key is handed out
   (`extract_escrow_public_key_pem`: the old extractor read RSA only, so a P-256 key would have been NotFound).
   *A refinement of item 3 above:* "each kind once" is the renewal rule's (`call_admit`, the same five-minute bound
   as the window), so no new ledger record; an escrow retrieval asked twice in the window gives the same key to the
   same custodian, and the escrow certificate and its store are once by their own rules (one service certificate
   per identity; the name never replaced).
3. The floor (`fcdc4128`): a custodian's floor is `CUSTODIAN_PUBLISHES` and `CUSTODIAN_SUBSCRIBES`, implied by the
   identity, nothing its certificate declares; the broker and every agent read it.
4. The daemon (`7ff1953a`, `3202cdaf`, and `032f6f99` for the client's `RegistrationRefused` event):
   `Slot::Custodian`, signing only the daemon's own handshake; `Bootstrap`, `Ready`, `Unwrap`; the escrow key
   (`polar_custody::escrow`), sealed only to the key the daemon pinned; the custodian's session, an ordinary one now,
   driving a pure state machine (`polar-custody/daemon/src/bootstrap.rs`); a registration refused as revoked
   destroying every key. Through a broker enforcing its floor with no policy, a custodian fetches and installs its
   workload's certificate (`cassini/broker/tests/custody_revocation.rs`).
5. `polar-init` (`9d51ee7a`): the custodian's certificate through the observer (or, for a bootstrap service, from
   cert-issuer with nothing in it), the plan, `Ready` within four minutes, the bundle from `Certificate`.
6. `polar-crypto` (`37000c3e`): the daemon holder, one opening path, no bootstrap with a daemon; and the recipient's
   key believed only for its certificate (`polar-crypto/src/recipient.rs`).
7. The bootstrap pods: in 1, 2 and 5 (their custodian from cert-issuer, held and vouched for as the service is).

**Step 8, on a fresh u7c (2026-10-28, `up.nu sandbox-local --dev` at `44766117`, stopped at the browser step as
asked; Mullvad off for the run):** what was checked, how, and what was not.
- *Every pod with a daemon bought only its custodian's certificate, and its custodian fetched the rest:* each
  polar-init logged the custodian's certificate installed and its plan; each daemon logged its session up and "the
  custodian has fetched everything". Plans: an empty one for Cassini, the credentials agent and its observer
  (bootstrap services, their own certificates from cert-issuer); a client certificate for each agent; the git server's
  server certificate with its name; the escrow key for kube-consumer and git-repo-observer. kube-consumer's took about
  six seconds from the custodian's certificate to Ready.
- *Escrow:* the credentials agent's own count: two retrievals (NotFound on a fresh cluster), two escrow issuances, two
  stores, and no refusal in its log.
- *No key and no token in a workload:* `ls /etc/tls/certs` in each workload container: `ca.pem` and `cert.pem` only,
  except cert-issuer (its key in process, decision 4) and polar-scheduler-observer, which has no daemon at all (a
  finding below). A service-account token is mounted in kube-observer's workload (its `kube-api-read`, by design) and
  cert-issuer's, nowhere else. No asymmetric key in `/proc/keys` in kube-consumer, on the node, or on the host.
- *X revoked:* kube-observer's client certificate (`2ddce4d1…`) revoked with the admin tool at 04:00:48.334: its daemon
  destroyed the key at .379 (`slots=[Client]`) and exited; the health actor entered rejuvenation at 04:01:04 ("the
  custody daemon holds no certificate"), and the pod ended at 04:03:04 (the Cassini-unreachable verdict at its 120 s
  deadline: the broker had closed the revoked session). Its replacement bought a new custodian and had its new client
  certificate (`236d746a…`) one second later.
- *The custodian revoked:* git-repo-processor's custodian (`27ccd6b5…`, read from the ledger) at 04:05:38.816: the
  daemon destroyed every key at .861 (`slots=[Custodian]`) and exited; the workload drained and ended through its
  health actor at 04:09:07; the replacement bought a new custodian and had its certificate at 04:10:18.
- *Not checked on the cluster, only in tests:* the workload refused its own escrow key (no client in a workload image
  to ask with), a custodian refused a second certificate or after its window (nothing in a pod asks twice), and a secret
  opened through the daemon (nothing in sandbox-local opens one; the secret harness is not in the target).
- *Findings, not chased:* polar-scheduler-observer has no custody daemon and holds `key.pem`; the consumer's dispatch
  logs "no handler for topic" for admin-action and account requests it has just handed to their actors (those two
  branches fall through to the warning, as before this work); jira-observer fails on the target's placeholder Jira, as
  before; the container tests (Neo4j, the registry) do not start on the host's new kernel (netavark's DNAT).

**Decided along the way (2026-10-27, the maintainer):** the custodian identity, instead of the token on a retrieval
or a daemon-signed ephemeral (2a); X's certificates from the credentials agent, not from cert-issuer; forced
rejuvenation kept; the custodian's authority spent, not kept. **Decided here, by precedent:** a new certificate type
(as `Signing` was), the name `custody.<X>`, and the five-minute window (`deployments.md`, item 2).

### Part B: the credentials agent's key

5. **A fourth slot, `Slot::Authority`, in the credentials agent's daemon only, loaded from the file.** The file moves to
   a volume only the daemon mounts (the ledger stays where it is: the PVC splits in two, `credentials-agent-pvc`, as
   the to-do entry says), readable by the daemon's UID alone, and the daemon generates it there if absent
   (`load_or_generate` moves into the daemon). It is the one key a daemon reads from disk, since it must outlive every
   pod; what changes is that the workload can no longer read it. Where the kernel can hold it, in the process keyring
   after loading, with the file's bytes zeroed.
6. **Nothing that signs anything else.** Its requests, the workload's:
   - `AuthoritySign { payload }`: the daemon signs `DOMAIN` and the payload itself, RSA-PSS SHA-256, so a signature is
     only ever an authority signature (as `Sign` signs only a TLS 1.3 CertificateVerify input);
   - `AuthorityCsr { identity }`: the CSR for the key's Signing certificate, built in the daemon;
   - `Decrypt { slot: Authority, ciphertext }`, as for an agent;
   - `PublicKey { slot: Authority }`.
   In the agent, an `EscrowKey` trait (public key, decrypt, sign an authority payload, CSR) in place of
   `Arc<EscrowKeypair>`; the file-held key implements it for tests, the daemon client in a pod. The publisher and the
   certifier take it in place of the `RsaPrivateKey`.
7. **What it protects.** The table above, for the root key: a compromised credentials agent can still sign while it runs
   (logged by the daemon), but cannot carry the key away. Root on the node can still read the file.

### Tests first, each part

- Properties: a ciphertext encrypted to the slot's public key decrypts through the daemon to what was encrypted, for any
  message the size allows, in the keyring and in memory; no sequence of requests from any role gets a reply holding the
  private key (as step 1's); `EscrowSeal`'s output opens only with the pinned authority key, whatever the caller sends;
  an `EscrowReceive` not sealed to the current ephemeral is refused and the ephemeral is gone; every role check
  (`Decrypt` and `AuthoritySign` not the installer's, the bootstrap requests not the workload's); after a revocation
  every escrow request is `Revoked` and the slot empty; an authority signature verifies with `authority_update::verify`
  and nothing the caller sends makes the daemon sign bytes without the `DOMAIN` prefix.
- Through `polar-init` and the credentials agent: retrieve, issue and escrow, a second pod retrieving what the first
  escrowed, all against the broker and the credentials agent as the integration tests run them now.
- On the cluster: no escrow key in any user keyring, no `escrow-key.pem` in the credentials agent's workload, the
  authority published and accepted, a secret retrieved and decrypted.

### What polar-init sees, today and planned

Today `polar-init` holds the agent's escrow key every time, not only at first install: on issuance its `CryptoActor`
generates the key and has the PEM in memory (`generate_csr` returns `private_key_pem`, `polar-crypto/src/lib.rs:1157`),
and on retrieval it opens the sealed PEM into its memory (`:1075`), before loading it into the keyring. Under the plan
it never holds it: the daemon generates it on first install and opens a retrieval itself. The key does leave the
daemon once, sealed to the credentials agent's key, since escrow is a second copy by design.

### The key type (decided: HPKE with P-256, the maintainer, 2026-10-27)

RSA was chosen for the kernel keyring, which this plan stops using, and it costs: RSA-4096 key generation takes
seconds, a decryption milliseconds. The agents' key only decrypts (others encrypt to it), so its natural replacement is
a KEM under HPKE (RFC 9180) in place of RSA-OAEP wrapping an AES key. The candidates:
- *HPKE with DHKEM(P-256, HKDF-SHA256), AES-256-GCM.* Every part NIST-approved (SP 800-56A ECDH, SP 800-56C, FIPS 197),
  though Polar uses no validated module (rustls runs on `ring`). The key can sign its own CSR (ECDSA), so issuance and
  the X.509 escrow certificate stay as they are. Not post-quantum.
- *HPKE with DHKEM(X25519).* The fastest and simplest; the retrieval ephemeral is X25519 already. Not in SP 800-56A.
  It cannot sign, so the CSR needs another proof of possession (the daemon binding it with the pod's mTLS key, which it
  holds).
- *A hybrid with ML-KEM-768 (FIPS 203)*, with X25519 (X-Wing) or P-256. Escrow is the case post-quantum KEMs are for:
  secrets kept for years, and ciphertexts on topics that can be recorded now and broken later. Costs: about 1.2 KB of
  public key and 1.1 KB per encapsulation, young crates (RustCrypto `ml-kem`, `x-wing`), no CSR (a KEM cannot sign),
  and X.509 for ML-KEM only in draft (`draft-ietf-lamps-kyber-certificates`), which rcgen does not produce.
- *P-256 now, in a versioned envelope that names its suite*, so that a hybrid suite is added later as a second suite,
  not a second migration.

The credentials agent's key (part B) does two jobs, so it splits in two: a signing key (Ed25519, FIPS 186-5, or ECDSA
P-256, both of which `polar-signing` already verifies) and a decryption key of the agents' type.

Whatever the type, the same things change: everything that encrypts to an escrow key (the credentials agent sealing a
retrieved secret, `polar-sign seal`, `EncryptForRecipient`) writes the new envelope; secrets stored under RSA stay
readable until re-encrypted; and an agent whose escrowed key is RSA is issued a new key.

### The crypto change, for HPKE with P-256

Read for it: the envelope is `polar-escrow`'s `EscrowedSecret` (`polar-escrow/src/lib.rs:77`): AES-256-GCM over the
payload, its key RSA-OAEP-wrapped to `encrypted_to_pem`, the binding context as AES-GCM associated data. The
credentials agent's key stays RSA until part B, so for the whole of part A the envelope carries both.
- *The envelope names its suite.* `EscrowedSecret` gains `suite`: `RsaOaep` (today's) or `HpkeP256` (RFC 9180, base mode, DHKEM(P-256, HKDF-SHA256), HKDF-SHA256, AES-256-GCM; the `hpke`
  crate, on the `p256` already in the workspace). The recipient's public key decides it on encryption: an RSA key gives
  `RsaOaep`, a P-256 key `HpkeP256`, anything else is refused. The two layers stay: HPKE seals the 32-byte AES key
  (`encrypted_key` is the encapsulated key and that sealed key), with a Polar `info` string, so the daemon opens
  32 bytes and never sees a payload, as the kernel keyring never did. The binding context stays the payload's
  associated data.
- *The daemon's request is `Unwrap { slot: Escrow, wrapped }`*, the HPKE open of a wrapped key. `DecryptOwnMessage`
  (a raw RSA-OAEP block today, `polar-crypto/src/lib.rs:237`) takes the envelope instead, as `DecryptOwnEscrowed` does.
- *Issuance.* The daemon's `EscrowCsr` makes a P-256 key; the key signs its own CSR, the proof of possession, as now.
  cert-issuer's `Service` certificate gives `KeyAgreement` for an EC key where it gives `KeyEncipherment` for RSA
  (`cert-issuer/server/src/ca.rs:299`), and `KeyAlgorithm::Rsa` stops being the escrow key's
  (`polar-crypto/src/lib.rs:1158`).
- *Every encrypter* (`EncryptForRecipient`, the credentials agent sealing a retrieved secret, `polar-sign seal`) goes
  through `EscrowedSecret::encrypt`, which picks the suite from the key, so none changes by itself; their tests gain
  the P-256 case.
- Properties: a round trip for each suite, for any payload and context; a secret opens under no other key, no other
  context and no other suite; a malformed or truncated envelope is refused, not a panic; the suite is decided by the key
  and by nothing in the request.
  *Built 2026-10-27* (`polar-escrow`, `tests/props.rs`): the suite in the secret, `encrypt` choosing it from the key
  (any other kind of key refused), `decrypt_p256`, `unwrap_p256` for a holder that only unwraps, and `decrypt_with`
  taking the holder's suite (refused before the holder is asked). A changed byte of the encoding's padding decodes to
  the same secret, and the tamper property says so. Breaking the suite check, the context binding, or the suite
  recorded for a P-256 key is each caught.

### Decided

- **Part A first** (the maintainer, 2026-10-27).
- **HPKE with P-256** for the agents' escrow keys (the maintainer, 2026-10-27), and for part B's decryption key.
- **No migration** (the maintainer, 2026-10-27): the change ships with a fresh install, and what was sealed to an
  agent's RSA key is sealed again (`polar-sign seal`). A daemon handed an RSA key from escrow refuses it, naming the key
  type.

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
   *Found for step 4 (2026-10-27):* a graph writer presents its one client certificate to Neo4j through neo4rs
   (`GRAPH_CLIENT_KEY=/etc/tls/certs/key.pem`, `infra/config/capabilities.nuon`). neo4rs reads the key from a file
   only (its `MutualTLS`, `lib/src/connection.rs`), and with the daemon there is none. Settled 2026-10-27: our neo4rs
   takes a rustls configuration ("The neo4rs patch").
4. The catalog and the charts: the sidecar in the `agent` and `service` kinds, its socket volume, its UID; Cassini's
   listener taking the signer.
5. Revocation: the daemon on its own serial, its exit, and the broker ending revoked sessions.
6. The speed gate: the reconnection storm in the load harness, with key files and with the daemon. *Closed
   2026-10-27: the benches measure it, and nothing gates on them ("Step 6, the speed gate").*
7. On a fresh sandbox-local: no `key.pem` in any pod; a revoked agent's session ends and its key is gone.
8. The escrow keys ("The escrow keys into the daemon"): its two parts, in the order decided there.
