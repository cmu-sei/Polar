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

Status, 2026-10-27: a design for the maintainer to clear. Nothing here is built.

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
  signer in `polar-crypto` (`rustls::sign::SigningKey`) sends what a handshake needs signed and returns the signature;
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

## Open, for the maintainer

1. **What a revoked pod does next.** Destroying its key ends its identity. Then either the daemon exits and the pod
   restarts (in place) and asks again, which the credentials agent can refuse, or it stays without a key until it is
   replaced. Recommendation: exit, so the pod's ending is visible and the credentials agent decides, by its own rules
   and the attestation agent's word, whether it is issued again.
2. **The broker ending revoked sessions.** It is the other half of immediacy, and Cassini is not on the builder.
   Recommendation: built with the daemon, the broker holding the revoked set from the same signed updates.
3. **The escrow key in the daemon too.** One place for every key a pod holds; `CryptoActor` asks the daemon to decrypt
   as the workload asks it to sign. Recommendation: after the mTLS key, as its own step.
4. **The bootstrap services.** Cassini and the credentials agent run `polar-init` and can carry the daemon like
   everyone; cert-issuer signs itself in process with no `polar-init` and keeps its key as now.

## Build order, each tests first

1. The daemon: key generation (process keyring or memory), the CSR, taking the certificate and judging it, signing,
   the socket and its caller check. Properties: a signature verifies under the certificate it holds; no message gets the
   key out; a caller that is not the pod's is refused.
2. The workload signer in `polar-crypto` (rustls), and the Cassini client and the HTTP identities taking it in place of
   `TLS_CLIENT_KEY`.
3. `polar-init` taking the daemon's CSR and handing it the certificate; the bundle in the shared volume becomes the
   certificate and `ca.pem` alone.
4. The catalog and the charts: the sidecar in the `agent` and `service` kinds, its socket volume, its UID; Cassini's
   listener taking the signer.
5. Revocation: the daemon on its own serial, and the broker ending revoked sessions.
6. On a fresh sandbox-local: no `key.pem` in any pod; a revoked agent's session ends and its key is gone.
