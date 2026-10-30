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

# The broker's baseline (2026-10-21)

What Cassini does with one producer and one subscriber, measured in the cluster by the load harness
(`src/agents/cassini/test`, `infra/load-harness.nu`; `policy-enforcement.md`, stage 5a), before stage 5b makes the broker
decide each publish and subscribe. It is the reference stage 5b is measured against, without and with its decision
cache. Measuring it found six faults, five in the broker and its client and one in how the harness used identities; four
are fixed and in the "after" figures, which are the baseline.

## The machine and the cluster

- Host: Intel Core i9-13950HX (24 threads), 62 GB, Linux 7.2.9, NixOS; `/home` is f2fs on LVM over a RAID 0 of two
  LUKS-encrypted NVMe drives. podman 5.8.8, rootless.
- Node: nix-usernetes `88215fc` (one node, `u7s-node`), Kubernetes 1.33.10, containerd 2.2.1.
- Target: `sandbox-local` (Flux), every catalog workload of it running, the load harness enabled (`db4ba7d6`).
- Producer, sink and broker on the one node, so the latencies compare one clock (each record says so: `one_node`).

## The procedure

In this order. The records are kept outside the repository (they hold for this machine only); the summaries are below.

1. The host: at least 20% of `/home` free (the kubelet taints the node at 15%: on 2026-10-21 a 135 GB Cargo `target`
   directory did it, and every pod was evicted); no VPN (with Mullvad up the node resolves nothing: its only working
   nameserver is the fourth, and resolvers use the first three).
2. A fresh cluster, from `~/Documents/projects/nix-usernetes`: `just reset && just up && just init && just kubeconfig`,
   then `export KUBECONFIG=~/Documents/projects/nix-usernetes/kubeconfig`.
3. From the repository root, with the dev shell's SOPS settings (`SOPS_AGE_BINARY` is `rage`,
   `SOPS_AGE_SSH_PRIVATE_KEY_FILE` is `~/.ssh/id_github`): `nu infra/up.nu sandbox-local --dev --user <user> --email
   <email>`. It stops at the browser step on a new cluster; with Librewolf closed, `just user-cert-install <user>`, then
   the same `up.nu` again finishes it. After a change to an image, the same command rebuilds and redeploys.
4. Tracing off, for the measuring: `kubectl -n flux-system patch kustomization polar-agents --type=merge -p
   '{"spec":{"suspend":true}}'` and `kubectl -n polar scale deploy jaeger --replicas=0`. Jaeger's all-in-one keeps every
   span in memory (10 GB within the hour), and with it up a run measures the broker and the export of a span a message.
   The agents' exporters then fail every few seconds, which is cheap.
5. `nu infra/load-harness.nu run infra/load-plans/baseline.nuon --target sandbox-local` from a clean tree (the record
   names the commit, whether the tree was dirty, the broker's image digest and each pod's node). About half an hour.
6. Tracing back: the same patch with `"suspend":false`; Flux scales Jaeger up again.

Running beside it: every other `sandbox-local` workload, mostly idle; Neo4j, into which kube-consumer writes the run's pods
as they come and go; Keycloak; and on the host clamd, at about half a core throughout.

The plan (`infra/load-plans/baseline.nuon`): each size at each rate, one producer and one sink on one lane; 20 seconds of
messages a run, at most 100,000 messages and 1 GiB; the first 100 left out of the latencies; a window of 1,000 messages
(and 64 MiB). "max" is as fast as the window allows.

## The figures

Received a second and MB/s are what the sink took in; latencies are milliseconds from send to receipt; "lost" is what the
sink never received.

- Before: `2bc47be7` (with the fix for the broker shutting down, without the four after it), broker
  `cassini@sha256:df962f53…`, record `~/.cache/polar/harness/baseline-20261021T173531.nuon`, its one-producer runs. Its
  window was 64 MiB only, so its "max" rows are not comparable (at 256 bytes that window let 262,144 messages be in
  flight: that row measured the queue).
- After, the baseline: `1020136a`, broker `cassini@sha256:fbe27f2a…`, record
  `~/.cache/polar/harness/baseline-20261021T193429.nuon`. Every pod reported, nothing was lost, repeated or out of order,
  in any run.

| size | rate | before /s | before MB/s | before p50 | before p99 | before lost | after /s | after MB/s | after p50 | after p99 | after max | after lost |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 256 B | 100 | 100 | 0.0 | 0.43 | 9.82 | 0 | 100 | 0.0 | 0.39 | 1.0 | 30.73 | 0 |
| 256 B | 1000 | 998 | 0.3 | 2.01 | 4.43 | 0 | 1000 | 0.3 | 2.06 | 4.78 | 5.69 | 0 |
| 256 B | 10000 | 9960 | 2.5 | 1.54 | 5.39 | 0 | 9999 | 2.6 | 1.97 | 9.97 | 12.95 | 0 |
| 256 B | max | 33130 | 8.5 | 1516.82 | 2909.83 | 0 | 13138 | 3.4 | 76.69 | 105.5 | 111.62 | 0 |
| 4 KiB | 100 | 100 | 0.4 | 0.81 | 1.22 | 0 | 100 | 0.4 | 0.51 | 0.83 | 1.37 | 0 |
| 4 KiB | 1000 | 1000 | 4.1 | 0.82 | 1.47 | 0 | 1000 | 4.1 | 0.48 | 0.88 | 1.57 | 0 |
| 4 KiB | 10000 | 3087 | 12.6 | 5279.2 | 5429.15 | 0 | 9998 | 41.0 | 2.19 | 13.26 | 17.44 | 0 |
| 4 KiB | max | 1976 | 8.1 | 8196.61 | 9875.99 | 29114 | 11957 | 49.0 | 85.45 | 106.76 | 113.68 | 0 |
| 64 KiB | 100 | 100 | 6.5 | 14.97 | 24.21 | 0 | 100 | 6.5 | 10.57 | 11.65 | 41.67 | 0 |
| 64 KiB | 1000 | 220 | 14.4 | 4641.56 | 4767.19 | 0 | 1000 | 65.5 | 0.6 | 2.26 | 4.2 | 0 |
| 64 KiB | 10000 | 145 | 9.5 | 4975.7 | 11400.77 | 0 | 9981 | 654.1 | 3.05 | 8.07 | 10.19 | 0 |
| 64 KiB | max | 216 | 14.1 | 4739.1 | 5098.32 | 0 | 9741 | 638.4 | 100.08 | 129.6 | 133.13 | 0 |
| 1 MiB | 100 | 14 | 14.6 | 4581.98 | 4959.51 | 0 | 100 | 105.0 | 3.26 | 14.59 | 24.37 | 0 |
| 1 MiB | 1000 | 14 | 14.7 | 4543.67 | 4999.97 | 0 | 997 | 1044.9 | 4.85 | 13.91 | 15.76 | 0 |
| 1 MiB | 10000 | 14 | 14.3 | 4672.1 | 4969.93 | 0 | 1456 | 1526.9 | 97.88 | 170.29 | 171.1 | 0 |
| 1 MiB | max | 14 | 14.5 | 4631.48 | 4695.88 | 0 | 1353 | 1418.7 | 160.67 | 226.9 | 227.44 | 0 |

Read plainly: one connection now carries about 10,000 small messages a second at a p50 of 2 ms, and about 1.4 GB/s of
1 MiB messages; before, anything above about 14 MB/s queued for seconds, whatever the message size. At "max" the latency
is the window over the rate (1,000 messages at 13,000 a second is about 77 ms): what saturation costs, with this window.

## Stage 5b: the broker deciding (2026-10-22)

The same machine, cluster and procedure, with the broker deciding every publish and subscribe (Cedar first, then the
certificate's floor; `policy-enforcement.md`, stage 5b), measured twice by the plan `infra/load-plans/enforcement.nuon`:
without the decision cache (`CASSINI_DECISION_CACHE_SECS` 0, every publish asks the broker's policy actor), and with its
default of five seconds. The harness restarts the broker with each setting (Flux suspended), and puts back the rendered
document at the end. Commit `cd57d2b0`, a clean tree, broker `cassini@sha256:3acfe504…`, record
`~/.cache/polar/harness/enforcement-20261022T134645.nuon`. Every pod reported, on one node; nothing was lost or refused.
The seeded policy says nothing about topics, so each decision asks Cedar, which is silent, and the floor admits it: the
whole cost of a decision is paid. Received a second, and p50 and p99 in milliseconds, for the baseline, without the
cache and with it:

| size | rate | baseline /s | no cache /s | cache /s | baseline p50 | no cache p50 | cache p50 | baseline p99 | no cache p99 | cache p99 |
|---|---|---|---|---|---|---|---|---|---|---|
| 256 B | 100 | 100 | 100 | 100 | 0.39 | 0.61 | 0.4 | 1.0 | 1.16 | 0.89 |
| 256 B | 1000 | 1000 | 1000 | 998 | 2.06 | 2.07 | 1.74 | 4.78 | 4.75 | 4.42 |
| 256 B | 10000 | 9999 | 5310 | 9996 | 1.97 | 93.64 | 1.81 | 9.97 | 131.68 | 10.49 |
| 256 B | max | 13138 | 5290 | 14371 | 76.69 | 95.13 | 68.03 | 105.5 | 132.74 | 98.67 |
| 4 KiB | 100 | 100 | 100 | 100 | 0.51 | 0.72 | 0.51 | 0.83 | 1.13 | 0.91 |
| 4 KiB | 1000 | 1000 | 1000 | 1000 | 0.48 | 0.82 | 0.53 | 0.88 | 1.39 | 0.91 |
| 4 KiB | 10000 | 9998 | 5686 | 10000 | 2.19 | 87.72 | 1.84 | 13.26 | 122.19 | 9.19 |
| 4 KiB | max | 11957 | 5652 | 13639 | 85.45 | 89.39 | 71.58 | 106.76 | 127.32 | 103.58 |
| 64 KiB | 100 | 100 | 100 | 100 | 10.57 | 10.8 | 10.53 | 11.65 | 12.07 | 20.18 |
| 64 KiB | 1000 | 1000 | 998 | 1000 | 0.6 | 1.63 | 0.82 | 2.26 | 3.72 | 3.32 |
| 64 KiB | 10000 | 9981 | 4753 | 10000 | 3.05 | 223.09 | 2.93 | 8.07 | 366.5 | 8.83 |
| 64 KiB | max | 9741 | 4811 | 9978 | 100.08 | 185.58 | 100.73 | 129.6 | 318.01 | 126.13 |
| 1 MiB | 100 | 100 | 100 | 100 | 3.26 | 2.98 | 2.47 | 14.59 | 15.97 | 13.89 |
| 1 MiB | 1000 | 997 | 997 | 995 | 4.85 | 5.57 | 4.34 | 13.91 | 13.09 | 11.9 |
| 1 MiB | 10000 | 1456 | 1328 | 1395 | 97.88 | 71.32 | 152.4 | 170.29 | 91.25 | 220.54 |
| 1 MiB | max | 1353 | 1283 | 1380 | 160.67 | 112.45 | 191.02 | 226.9 | 133.42 | 255.08 |

Read plainly:
- **Deciding every publish costs about 0.2 ms, one at a time.** Without the cache one connection carries at most about
  5,300 small messages a second (4,800 of 64 KiB), against 10,000 to 13,000 before: the Listener waits for each decision
  before the next publish, and the policy actor decides one at a time. Below that rate nothing changes but a few tenths
  of a millisecond of p50. At 1 MiB the bytes dominate and the decision does not show.
- **With the cache the broker is where it was.** Every row is within the baseline's run-to-run spread, the saturated
  small-message rows a little above it. A connection decides each topic once every five seconds, and a change of
  authority ends every kept decision at once (stage 5b, slice 3), so the default stays.
- The 1 MiB rows at 10,000 a second and "max" are the window over the rate in both passes, as in the baseline, and move
  by tens of milliseconds from one run to the next; they say nothing about the decision.

The local load test of slice 2 agrees: four publishers deciding every publish took 12.5 s, against 1 s before the broker
decided and 1.5 s with the cache.

## A second machine: whitey (2026-10-25)

The same plans on another workstation and a newer node, three days later, as a reference for that machine. The broker
now always decides (stage 5b), so there is no broker that decides nothing to measure: `baseline.nuon` measures it as
deployed, with its default five-second decision cache, and `enforcement.nuon` again without the cache and with it. The
baseline and "cache" columns below are therefore the same settings measured twice, which shows the run-to-run spread.

- Host: AMD Ryzen 9 9950X3D (32 threads), 92 GB, Linux 7.3.0 (a hardened, clang-built kernel, FIPS mode, no IPv6);
  `/home` is f2fs on LVM over a RAID 0 of two self-encrypting NVMe drives. podman 5.8.9, rootless.
- Node: nix-usernetes `f7a3c49` (one node), Kubernetes 1.37.1, containerd 2.4.1, kube-proxy in nftables mode.
- Target: `sandbox-local`, brought up fresh from `5a62a813` by `up.nu --dev` from the dev container in the cluster;
  every catalog workload of it running (the Jira observer failing, as it has no Jira site), the load harness enabled.
- Procedure as above, run from the dev container's clean checkout (`dirty: false`), with tracing off. Mullvad was
  connected; the harness's traffic stays in the cluster.
- Records: `~/.cache/polar/harness/baseline-20261025T213412.nuon` and `enforcement-20261025T214002.nuon`, broker
  `cassini@sha256:fec1df50…`. Every pod reported, on one node; nothing was lost, refused or failed, in any run.

| size | rate | baseline /s | no cache /s | cache /s | baseline p50 | no cache p50 | cache p50 | baseline p99 | no cache p99 | cache p99 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 256 B | 100 | 100 | 100 | 100 | 0.11 | 0.34 | 0.12 | 0.52 | 20.53 | 0.51 |
| 256 B | 1000 | 998 | 998 | 998 | 1.38 | 1.62 | 1.29 | 4.6 | 5.03 | 4.59 |
| 256 B | 10000 | 9999 | 9960 | 9960 | 0.61 | 1.62 | 0.62 | 2.04 | 3.81 | 2.12 |
| 256 B | max | 20095 | 10914 | 20964 | 48.93 | 45.83 | 47.77 | 62.22 | 62.87 | 56.25 |
| 4 KiB | 100 | 100 | 100 | 100 | 0.23 | 0.47 | 0.22 | 0.78 | 1.19 | 0.76 |
| 4 KiB | 1000 | 1000 | 1000 | 1000 | 0.1 | 0.37 | 0.15 | 0.2 | 0.61 | 0.28 |
| 4 KiB | 10000 | 10000 | 9999 | 10000 | 0.55 | 1.39 | 0.55 | 1.25 | 3.21 | 1.24 |
| 4 KiB | max | 19842 | 10830 | 20099 | 50.11 | 47.22 | 49.45 | 67.2 | 62.19 | 73.04 |
| 64 KiB | 100 | 100 | 100 | 100 | 10.54 | 10.73 | 10.54 | 19.75 | 19.89 | 20.33 |
| 64 KiB | 1000 | 998 | 998 | 1000 | 1.38 | 0.59 | 0.38 | 2.62 | 2.97 | 2.62 |
| 64 KiB | 10000 | 10001 | 9894 | 10001 | 0.73 | 3.46 | 0.7 | 1.73 | 19.9 | 1.74 |
| 64 KiB | max | 16365 | 9612 | 16755 | 58.98 | 59.57 | 59.74 | 63.09 | 106.92 | 62.4 |
| 1 MiB | 100 | 100 | 100 | 100 | 1.69 | 2.11 | 1.61 | 12.82 | 13.64 | 12.7 |
| 1 MiB | 1000 | 1004 | 1002 | 1003 | 1.2 | 1.17 | 1.48 | 3.05 | 2.83 | 3.23 |
| 1 MiB | 10000 | 2792 | 2391 | 2418 | 31.55 | 28.62 | 44.12 | 34.37 | 36.54 | 72.85 |
| 1 MiB | max | 2455 | 2335 | 2207 | 64.48 | 32.76 | 72.76 | 107.69 | 38.75 | 89.76 |

Read plainly:
- **One connection carries about 20,000 small messages a second**, and about 2.9 GB/s of 1 MiB messages; at the set
  rates up to 10,000 a second the p99 stays within a few milliseconds.
- **Deciding every publish costs about 0.04 ms here.** Without the cache one connection carries at most about 10,900
  small messages a second (9,600 of 64 KiB), about 92 µs a message against 50 µs with it. As before, below that rate
  only a few tenths of a millisecond of p50 change, and at 1 MiB the bytes dominate.
- **With the cache the broker is at its baseline**: the two columns with the same settings differ by less than the
  saturated rows move between runs.
- The 1 MiB rows at 10,000 a second and "max" are again the window over the rate, and move by tens of milliseconds
  from one run to the next.
- Against the 2026-10-22 figures, on a different CPU and node, every ceiling is higher (13,000 to 14,000 small messages
  a second then, 5,300 without the cache, 1.4 GB/s at 1 MiB): a reference for this machine, not a comparison of code.

## With the custody daemons (2026-10-27, u7c)

The same plan with every harness pod's key in its custody daemon (`key-custody.md`), as the catalog now deploys it,
on precisionws, to compare with its key-file baseline of 2026-10-21 above.

- Host: precisionws as above, Linux 7.3.0, FIPS on.
- Cluster: nix-usernetes `feaa20a`, multi-node `u7c` (`cp`, `core`), Kubernetes 1.37.1; every harness pod and the
  broker on `cp`, so one clock (`one_node: true`).
- Target: `sandbox-local` from `cce2438e` (clean), by `up.nu --dev`; tracing off for the run, as the procedure says.
  Record `~/.cache/polar/harness/baseline-20261027T194643.nuon`, broker `cassini@sha256:0931a806…`.
- Every Job ran to its end with the daemon as a native sidecar beside it, and the harness read its result past the
  daemon's container; each run's pod is new, so the renewal rule's five minutes never held one back.

| size | rate | key files /s | daemon /s | key files p50 | daemon p50 | key files p99 | daemon p99 | lost |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 256 B | 100 | 100 | 100 | 0.39 | 0.51 | 1.0 | 10.12 | 0 |
| 256 B | 1000 | 1000 | 998 | 2.06 | 2.1 | 4.78 | 5.02 | 0 |
| 256 B | 10000 | 9999 | 9960 | 1.97 | 1.7 | 9.97 | 11.49 | 0 |
| 256 B | max | 13138 | 11416 | 76.69 | 86.85 | 105.5 | 105.16 | 0 |
| 4 KiB | 100 | 100 | 100 | 0.51 | 0.53 | 0.83 | 0.88 | 0 |
| 4 KiB | 1000 | 1000 | 1000 | 0.48 | 0.46 | 0.88 | 0.83 | 0 |
| 4 KiB | 10000 | 9998 | 9998 | 2.19 | 1.74 | 13.26 | 10.94 | 0 |
| 4 KiB | max | 11957 | 11940 | 85.45 | 84.83 | 106.76 | 104.77 | 0 |
| 64 KiB | 100 | 100 | 100 | 10.57 | 10.51 | 11.65 | 11.85 | 0 |
| 64 KiB | 1000 | 1000 | 1000 | 0.6 | 0.77 | 2.26 | 3.39 | 0 |
| 64 KiB | 10000 | 9981 | 9995 | 3.05 | 2.94 | 8.07 | 7.83 | 0 |
| 64 KiB | max | 9741 | 10893 | 100.08 | 89.37 | 129.6 | 113.12 | 0 |
| 1 MiB | 100 | 100 | 100 | 3.26 | 3.11 | 14.59 | 14.47 | 0 |
| 1 MiB | 1000 | 997 | 967 | 4.85 | 6.36 | 13.91 | 14.33 | 0 |
| 1 MiB | 10000 | 1456 | 1231 | 97.88 | 143.27 | 170.29 | 212.51 | 0 |
| 1 MiB | max | 1353 | 1502 | 160.67 | 138.0 | 226.9 | 163.75 | 0 |

Read plainly: the same, within one run's spread. The daemon signs once for a connection, and a run is one long
connection, so nothing it does is in these figures, as expected (`key-custody.md`, "Step 6"). What differs is the cluster
(two nodes, Kubernetes 1.37 against 1.33, six days of other changes), seen most at 1 MiB, where one run is either side of
the other. One p99 stands out, 10 ms at 256 B and 100 a second: one slow message in 1,900, a p99 that is nearly the
maximum. No duplicates, no messages out of order, in any run.

## musl and the allocators: the plan (2026-10-28, cleared)

*Why:* static musl binaries are on the maintainer's list, for both amd64 and arm64, and they are what a seccomp filter
applied at load needs (no dynamic loader mapping libraries executable at start; `runtime-attestation.md`). musl's own
allocator (mallocng) is small and hardened, not built for many threads allocating at once, and the broker allocates on
every message (each frame, each payload's `Arc`, each ractor message, each message queued for a subscriber). The
maintainer: tests before deciding anything, starting with Cassini, the most performance-critical part and the one with
the most figures. Nothing is decided by this plan; it measures.

*The variants,* the broker alone changed; the producer and the sink stay as built today, so a run differs by one thing:

| | libc | allocator | why |
|---|---|---|---|
| V0 | glibc | glibc's malloc | the baseline, as deployed |
| V1 | musl | mallocng | musl as it comes, hardened |
| V2 | musl | mimalloc | fast, multi-threaded |
| V3 | musl | jemalloc (`tikv-jemallocator`) | fast, multi-threaded |
| V4 | glibc | mimalloc | tells the libc's effect from the allocator's |
| V5 | musl | Scudo | a hardened allocator built for threads (if its crate does not build on musl, that is the finding) |

*How they are built,* on an experiment branch, not merged unless a variant is chosen:
- The musl build of `cassini-server` as `polar-tools` already builds its static tools (`src/agents/polar-tools/package.nix`:
  the triple from `system`, crane with the musl target, `pkgsStatic`'s `cc`, OpenSSL linked in). The broker's own TLS is
  rustls with `ring`; OpenSSL reaches it only through `reqwest`'s default `native-tls` (`credentials-agent-client`, the
  `polar` crate), off the hot path, and links static as it does for the tools.
- The allocator as a `#[global_allocator]` behind a cargo feature of the server binary (`alloc-mimalloc`,
  `alloc-jemalloc`, `alloc-scudo`), one per image. The dependencies stay on the branch.
- Each image pushed to the in-cluster registry under its own tag (`cassini:exp-v<n>`) and set on the broker's Deployment
  with Flux's `polar-agents` suspended, as the procedure already has it; the harness records the image digest it ran.

*Correctness first:* Cassini's tests pass on the musl target (the broker, the client and the local load tests) before
any musl broker is measured, and a musl broker registers every agent and serves every subscription on the cluster
before the harness runs.

*What is measured:*
- The harness's figures, per run (`infra/load-plans/baseline.nuon`, 16 runs, about 30 minutes a variant).
- The broker's CPU seconds and memory (working set at each run's end, and its peak) per run, from the kubelet's summary
  (`/api/v1/nodes/<node>/proxy/stats/summary`, no exec into the broker). The harness does not record these yet: an
  addition to `infra/load-harness.nu`, tested first in `infra/tests/load-harness.nu`. CPU per message received is the
  figure where allocators differ most when the rate is set and the throughput cannot.
- The image's size, incidentally.

*The procedure:* this document's, on precisionws and the five-node `u7c` with `sandbox-local`, once the arm64 publish has
ended (its emulated builds would load the CPU through every run). The broker is on `core`, the harness's pods on
`services` (`placement.md`), so `one_node` is false: the nodes are containers on one host, sharing its clock, to be
confirmed before the latencies are read as one clock's. The order interleaved, two rounds, V0 to V5 and
then V0 to V5 again, so a drift through the session is not read as a difference: about six hours for the six.
V4 and V5 are in (the maintainer, 2026-10-28).

*Read with a rule set before the figures* (the maintainer to clear it):
- A row's spread is the difference between V0's two runs of it.
- A variant is *no worse* on a row when the mean of its two runs is within that spread of V0's mean, for throughput and
  for p99; *acceptable* when it is no worse on every row, and its CPU per message and memory are no worse by the same
  rule.
- A variant is *better* where it is beyond the spread: the saturated rows ("max", and 10,000 a second), CPU per message.
- Nothing is decided from one machine: the chosen variant is measured again on whitey, and arm64 is seen only where an
  arm64 node runs it (the tester's MacBook, as a smoke test, not a measurement).

*Then:* the figures here, a recommendation, and the maintainer's decision; the agents follow the broker only after it.

*Cleared (the maintainer, 2026-10-28):* the plan, with V4 and V5, and the rule as written. *The maintainer's prediction,*
on record before any figure: "it's all 'in the noise' and for our purposes, it won't matter which configuration we
use." Where a difference would show first, if anywhere: CPU per message at the set rates (the throughput is held by
the rate, the work to reach it is not), then the small-message "max" rows; the large-message "max" rows are the window
over the rate, as above, and would hide an allocator.

## musl and the allocators: the results (2026-10-28 and 29)

*How it was run.* precisionws and the five-node `u7c`, `sandbox-local`, the experiment branch at `9d008b6e` (clean); the
six brokers of `src/agents/cassini/experiment.nix`, all release builds, pushed as `cassini:exp-v0` to `exp-v5`. Two
rounds of the baseline plan, V0 to V5 and V0 to V5 again, 23:05 to 02:25, each variant set on the broker's Deployment and
checked first (the right image running, the agents' sessions back, nothing refused); every check passed, and every run
ended on the broker it began with. Records: `~/.cache/polar/harness/baseline-20261028T230839.nuon` to
`baseline-20261029T021055.nuon`, one a variant and round. Producer and sink both on `services`, the broker on `core`.

*A first attempt was discarded.* The broker belongs to Flux's `polar-cassini`, not `polar-agents`, and the procedure above
suspends only `polar-agents`: `polar-cassini` put the rendered broker back every ten minutes, partway through runs. The
second attempt suspended both. (The harness's own guard in `set_broker` checks only `polar-agents` too: to fix.)

| | V0 glibc | V1 musl, mallocng | V2 musl, mimalloc | V3 musl, jemalloc | V4 glibc, mimalloc | V5 musl, Scudo |
|---|---|---|---|---|---|---|
| 1 MiB at 10,000/s, received | 1,352, 1,254 | 775, 737 | 1,610, 1,555 | 1,601, 1,565 | 1,605, 1,620 | 751, 750 |
| 1 MiB, CPU a message | 1.3 to 1.6 ms | 2.2 to 2.4 ms | 1.3 to 1.5 ms | 1.3 to 1.5 ms | 1.2 to 1.4 ms | 2.1 ms |
| 256 B saturated, received | 13,889, 12,689 | 12,166, 10,960 | 12,022, 13,424 | 11,047, 11,014 | 13,479, 11,059 | 12,846, 10,917 |
| working set at the end | 293, 362 MB | 20, 21 MB | 159, 181 MB | 437, 364 MB | 149, 165 MB | 36, 38 MB |
| peak | 366, 521 MB | 233, 42 MB | 323, 523 MB | 552, 481 MB | 374, 331 MB | 51, 53 MB |
| messages lost | 0 | **167,751, in two stalls** | 0 | 0 | 0 | 0 |
| rows by the rule: same, better, worse (of 48) | | 12, 9, 27 | 23, 10, 15 | 24, 10, 14 | 26, 12, 10 | 17, 5, 26 |

(The full per-row verdicts, with each row's spread, are in `~/.cache/polar/harness/exp-verdicts.nuon` beside the
records.)

*The rule, as cleared, applied as written:* no variant is acceptable, since each is worse than V0 beyond V0's spread on
some row. Read plainly, two kinds of "worse" are in that count:
- **Real, and large:** the two hardened allocators, V1 and V5, give up about half the throughput of 1 MiB messages
  (about 750 a second against 1,250 to 1,625), cost 40 to 70% more CPU a message on large messages and 10 to 40% on
  small ones, and V1 stalled twice. The fast allocators, V2 to V4, are within a few percent of V0 on throughput and CPU
  almost everywhere.
- **The rule's, with two runs:** where V0's two runs agreed almost exactly (the set rates' throughput, 1,000.1 and
  1,000.1), any difference is "beyond the spread": V2 at 998.8 a second for 64 KiB at 1,000 is "worse" by 0.1%. Two runs
  understate the noise; most of V2 to V4's "worse" rows are under 5%, and the 256 B saturated row, where V0 itself moved
  by 1,200, carries most of the rest.

*Memory is where they differ most*, the other way round. (Corrected 2026-10-29: this said "mimalloc holds about half what
glibc does", which is true only after the heaviest runs.) The broker's working set after each run, in MB, rounds 1 and 2,
from the records' `broker_usage` (selected rows):

| load | V0 glibc | V1 mallocng | V2 musl, mimalloc | V3 jemalloc | V4 glibc, mimalloc | V5 Scudo |
|---|---|---|---|---|---|---|
| 256 B at 100/s | 14, 15 | 13, 13 | 75, 98 | 175, 176 | 130, 78 | 20, 22 |
| 4 KiB at 1,000/s | 24, 28 | 16, 17 | 129, 117 | 179, 184 | 115, 100 | 26, 27 |
| 64 KiB at 10,000/s | 42, 36 | 18, 20 | 142, 139 | 207, 202 | 128, 143 | 34, 35 |
| 1 MiB at 100/s | 70, 63 | 21, 20 | 159, 159 | 214, 216 | 132, 146 | 37, 37 |
| 1 MiB at 10,000/s | 160, 344 | 20, 20 | 152, 157 | 496, 424 | 140, 152 | 38, 38 |
| 1 MiB saturated | 293, 362 | 20, 21 | 159, 181 | 437, 364 | 149, 165 | 36, 38 |

And the peak, cAdvisor's `container_memory_max_usage_bytes`, the most the container has used since it started (so it only
rises through a round): after the small-message rows V0 39, 37; V1 28, 29; V2 142, 144; V3 191, 197; V4 147, 119; V5
37, 40. After every row: V0 366, 521; V1 233, 42; V2 323, 523; V3 552, 481; V4 374, 331; V5 51, 53.

So mimalloc holds three to six times what glibc does under light load, and peaks no lower under heavy load; Scudo stays
between 20 and 38 MB at every load and never passes 53. Part of Scudo's low peak is its lower throughput (fewer large
messages in flight at once), but where every variant carries the same load (64 KiB at 10,000 a second, 1 MiB at 100) it
is still at or below glibc. The broker is memory-bound where it will be installed (the maintainer, 2026-10-29: people will
run it on machines with little memory, and less memory under heavy load is a stability gain), so memory, not throughput,
decides.

*The maintainer's prediction* ("all in the noise; it won't matter which configuration we use") holds for the fast
allocators on musl or glibc: V2, V3 and V4 do not differ from glibc in what the broker carries. It does not hold for the
hardened ones, which cost large messages half their throughput, or for memory.

*Found on the way, each its own item:*
- **V1's stalls.** Twice a producer's acknowledgements stopped mid-run (4 KiB at 10,000 a second in round 1, 4 KiB
  saturated in round 2), with nothing refused, nothing logged and the broker up; the producer gave up after its 30 s
  stall timeout. In round 2, two revocations ("every cached decision is stale") came within the producer's first second,
  with kube-consumer rejuvenating. The broker decides each publish before reading the next (`listener.rs:694`), and after
  a revocation asks the policy actor with no deadline (`ractor::rpc::call(..., None)`, `polar-agent-builder/src/authority.rs:367`):
  a policy actor that is slow or stuck holds the connection silently. A likely mechanism, not a cause found; it may be
  a race every variant has, at odds mallocng's slowness raises. To reproduce, test first, before anything is changed.
- **Rejuvenations land in runs.** Six or eight agents renew together every 58 minutes or so (their pods began together);
  each time they re-register with the broker inside some run. Small, and in every baseline before this one too.
- **In the broker's log:** a client retrying every second with an expired certificate; handshakes with no certificate at
  all; `kubernetes.cluster.consumer.Pod` and `polar.provenance.discovery` queueing with nobody subscribed.

*Not decided here:* which build the broker uses. One more candidate the results suggest: mimalloc's secure mode (guard
pages, encrypted free lists), hardened at what its authors put at about 10%, which would be V6.

## The second set: Scudo from LLVM 23, and smalloc (2026-10-29)

*Why.* Memory decides (above), and on memory Scudo is the best of the first set; it cost large messages half their
throughput. The `scudo` crates (0.1.3 and 0.2.2, the last releases, 2022-10-24) carry the Scudo of that year, with
upstream's generic configuration fixed in. nixpkgs' LLVM 23.1.0 carries the current one, which takes a configuration of
its own (`SCUDO_USE_CUSTOM_CONFIG`, the fields in `allocator_config.def`) and options at run time (`mallopt`:
`M_CACHE_SIZE_MAX`, `M_CACHE_COUNT_MAX`, `M_DECAY_TIME`, `M_PURGE`, `M_TSDS_COUNT_MAX`). The likely cost of large messages
is the large-block cache: upstream's default keeps no freed block over 512 KiB (`DefaultMaxEntrySize = 1UL << 19`), so
every 1 MiB message is a fresh `mmap`, its page faults and a `munmap`; musl's mallocng maps large blocks the same way, and
V1 fell to the same rate. The maintainer asked for Scudo's tunables to be tried, to bring it near mimalloc and glibc, and
for smalloc (crates.io `smmalloc` 7.6.13, pure Rust, a slab a thread, no global lock) to be measured as it is.

*The builds.* Every Polar binary is musl with mimalloc now (`99d41b81`), so the control is the broker every bring-up runs.

| | build | allocator |
|---|---|---|
| V0 | glibc | its own malloc (the reference) |
| V2 | musl | mimalloc (the control) |
| S0 | musl | Scudo of LLVM 23.1.0, upstream's default configuration (`polar-scudo/configs/default`) |
| S1 | musl | S0 with the large-block cache keeping blocks to 4 MiB, the quarantine compiled out (`configs/cache`) |
| S2 | musl | S1 with thread caches shared (at most 8, as Android), freed memory released after 1 s, the futex condition variable (`configs/lean`) |
| M0 | musl | smalloc as published |

Scudo is compiled from nixpkgs' source by `polar-scudo` (build.rs: upstream's sources and flags, without exceptions, RTTI
or the C++ library, the wrappers named `polar_scudo_*` so musl's own malloc keeps its name). Its tests run on it as their
global allocator: alignment, zeroing and `realloc` as property tests, threads freeing each other's blocks, and which
options each configuration takes; four mutants of the allocator were each caught. Where the source comes from for good
(vendored, or nixpkgs) is decided only if Scudo is chosen. `cassini/experiment.nix` builds the six images.

*The procedure.* As the first set's, on u7c with every other Polar binary on musl and mimalloc: both `polar-agents` and
`polar-cassini` suspended, each variant set on the broker's Deployment and checked (its image running, the agents'
sessions back, nothing refused), the baseline plan's 16 runs, two rounds, V0 S0 S1 S2 M0 V2 and again. No acceptance rule
this time: every row's numbers are read as in the first set, memory first, then what S1 and S2 recover of throughput and
CPU against V2 and V0. If smalloc shows an obvious lever (it has no options; its source is about 700 lines), a tuned M1
is a later set.

## The second set: the results (2026-10-29)

*How it was run.* precisionws and `u7c`, `sandbox-local` brought up from `99d41b81` (every binary musl and mimalloc); the
six brokers built from `e97f7708` and pushed as `cassini:exp2-<variant>`. Two rounds, V0 S0 S1 S2 M0 V2 and again, 06:23
to 10:03. Every check passed (the right image running, 23 to 28 sessions back, nothing refused), every run ended on the
broker it began with, and nothing was lost. Records: `~/.cache/polar/harness/baseline-20261029T062600.nuon` to
`baseline-20261029T094452.nuon`, one a variant and round. Rounds 1 and 2 are given in that order throughout.

| | V0 glibc | S0 Scudo, default | S1 Scudo, cache | S2 Scudo, lean | M0 smalloc | V2 musl, mimalloc |
|---|---|---|---|---|---|---|
| working set at the end | 63, 95 MB | 30, 42 MB | 65, 56 MB | **46, 47 MB** | 1,910, 2,063 MB | 179, 159 MB |
| peak after the small-message rows | 45, 60 MB | 37, 47 MB | 37, 49 MB | **33, 45 MB** | 1,090, 1,199 MB | 123, 156 MB |
| peak after every row | 139, 152 MB | 97, 59 MB | 85, 76 MB | **58, 69 MB** | 1,917, 2,074 MB | 223, 193 MB |
| 1 MiB at 10,000/s, received | 1,672, 1,800 | 798, 779 | 1,616, 1,697 | 1,695, 1,789 | 1,384, 1,519 | 1,697, 1,524 |
| 1 MiB saturated, received | 1,736, 1,736 | 813, 744 | 1,539, 1,693 | 1,669, 1,503 | 1,379, 1,451 | 1,639, 1,606 |
| 1 MiB, CPU a message | 1.2 to 1.4 ms | 2.0 to 2.1 ms | 1.3 to 1.7 ms | 1.3 to 1.5 ms | 1.4 to 1.7 ms | 1.3 to 1.8 ms |
| 1 MiB saturated, p99 | 50, 56 ms | 130, 159 ms | 59, 51 ms | 52, 52 ms | 57, 56 ms | 56, 78 ms |
| 256 B saturated, received | 13,430, 12,075 | 10,971, 12,939 | 10,247, 11,887 | 10,143, 10,509 | 12,423, 12,639 | 12,519, 12,763 |
| 256 B at 10,000/s, CPU a message | 104, 101 µs | 121, 118 µs | 122, 125 µs | 125, 134 µs | 149, 155 µs | 113, 118 µs |
| messages lost | 0 | 0 | 0 | 0 | 0 | 0 |

The working set after each run, in MB (selected rows, as the first set's):

| load | V0 glibc | S0 | S1 | S2 | M0 smalloc | V2 mimalloc |
|---|---|---|---|---|---|---|
| 256 B at 100/s | 17, 33 | 20, 28 | 19, 26 | 17, 26 | 985, 1,126 | 98, 95 |
| 4 KiB at 1,000/s | 30, 46 | 24, 34 | 23, 32 | 23, 32 | 1,072, 1,179 | 107, 134 |
| 64 KiB at 10,000/s | 43, 54 | 28, 41 | 35, 38 | 24, 34 | 1,342, 1,471 | 132, 168 |
| 1 MiB at 100/s | 70, 74 | 30, 41 | 40, 45 | 23, 35 | 1,508, 1,585 | 167, 171 |
| 1 MiB at 10,000/s | 70, 75 | 31, 41 | 52, 68 | 41, 56 | 1,801, 1,910 | 175, 160 |
| 1 MiB saturated | 63, 95 | 30, 42 | 65, 56 | 46, 47 | 1,910, 2,063 | 179, 159 |

(Every row, for received, p99, CPU and working set, is in the records; the script that tabulates them is not kept.)

*Memory.* S2 holds the least of any build at every load: 17 to 56 MB after each run, a peak of 58 and 69 MB, under half
glibc's (139, 152) and about a third of mimalloc's (223, 193). S0 holds less still in its working set (30 to 42 MB), but
peaks higher (97 in round 1), and carries half the large messages. S1 sits between them.

*The large-block cache is the lever.* S0 repeats V5 of the first set: about 780 to 810 1 MiB messages a second, 2.0 ms
of CPU each, a p99 of 130 to 159 ms when saturated. Keeping freed blocks to 4 MiB (S1, S2) brings each back to the fast
builds: S2 carries 1,503 to 1,789 a second at 1.3 to 1.5 ms (V0 1,672 to 1,800 at 1.2 to 1.4; V2 1,524 to 1,697 at
1.3 to 1.8), with V0's p99 (52 ms). Sharing the thread caches and releasing freed memory after a second (S2 against S1)
costs large messages nothing and takes the peak down by about a fifth.

*What it still costs: small messages.* At the set rates every build keeps up (received equals offered up to 10,000 a
second at every size). Saturated with 256 B messages, S2 carries 10,143 and 10,509 a second against V0's 13,430 and
12,075 and V2's 12,519 and 12,763, 15 to 20% fewer; and small messages cost 10 to 30% more CPU than on glibc (256 B at
10,000 a second: 125, 134 µs against 104, 101, and V2's 113, 118). S0 and S1, whose thread caches are not shared, cost
the same (118 to 125 µs), so the sharing is not the cause; most likely it is Scudo's own work on each allocation (the
chunk header's checksum among it), which was not measured apart. The broker is already faster than it needs to be by a
wide margin (the maintainer, 2026-10-29), so this is the cost to weigh against the memory.

*smalloc: no.* It holds 985 and 1,126 MB after the first and lightest run (256 B at 100 a second) and 1.9 and 2.1 GB by
the end, ten times mimalloc and thirty times S2, and carries fewer large messages (1,379 to 1,519 a second). On Linux it
maps one region of about 70 TB of address space at start (`src/i/plat.rs`, `mmap` with `NORESERVE`) and has no
`madvise` or `munmap`: memory it has touched is never given back. It has no options; nothing in it is a lever on that
short of changing its design, so there is no M1.

*Between the sets.* V0 held less here than in the first set (63 and 95 MB at the end against 293 and 362) and carried more
1 MiB messages (1,672 to 1,800 against 1,254 to 1,352), on a cluster brought up afresh with every other binary on musl;
V2's numbers moved less (179, 159 against 159, 181). glibc's malloc is the one that moved; why is not looked into. Only
numbers from the same set are compared above.

*Found on the way:* `polar-scheduler-observer` restarts whenever the broker does (12 times in the set, against its
neighbours' 3, their hourly renewals): its health actor ends it on "cassini disconnected" where the other agents
reconnect. Its own item.

*Not decided here:* which build the broker uses. On memory, S2; on throughput of large messages, S2 is level with glibc and
mimalloc; the price is 15 to 20% of small-message throughput at saturation and 10 to 30% of the CPU they cost.

## Where the small-message time goes: a profile (2026-10-29, u7c)

The maintainer, 2026-10-29: the CPU is not what matters (Polar's uses are rarely compute-bound), the throughput is. To
see what holds S2's 256 B messages back, the broker was profiled under them: the scratch plan `profile-256` (256 B as fast
as the window allows, 1,000,000 messages), `perf record -e cpu-clock` on the broker's process from inside podman's user
namespace (the broker runs as a sub-uid; the hardware counters do not open from there), S2 and V0, one run each.

*No thread is busy.* Both builds spread about 1.2 cores over seven tokio workers, none above 16%; a message's CPU (about
100 µs) times the rate is the 1.2 cores. The work on a message is close to serial, through the listener, session,
topic and subscriber actors in turn, so a message's CPU sets the rate: whatever is taken out of it comes back as
throughput.

| share of the broker's CPU | S2 | V0 |
| --- | --- | --- |
| tracing (the registry, the OpenTelemetry layer, the filters) | 37% | 42% |
| the allocator | 15% | 7.5% |
| `memcpy` (musl's, glibc's) | 5.2% | 2.3% |
| tokio, syscalls | 23% | 28% |
| the broker's and ractor's own code | 10% | 11% |

*Tracing was the most of it,* whatever the allocator: the `info` span each publish opened was exported. Fixed on
`feat/ace-integration` (item 7 below; `5c68724f`, the trace filter `warn` by default, and `022a249e`, the span at
`debug`), and cherry-picked here. With the filter at `warn` alone, one run each: V0 from 11,580 to 19,027 a second (96 to
50 µs a message), S2 from 10,984 to 13,151 (108 to 70 µs). With tracing out of the way, Scudo's share is larger:

- *The shared thread caches' lock:* `HybridMutex::tryLock` and `unlock`, 7.8% of S2's CPU at `warn`, about 5 µs a
  message. S2's caches are shared (`TSDRegistrySharedT`), so every allocation and free takes one; S1's, one a thread,
  take none, at about a quarter more memory at the peak (85 and 76 MB against 58 and 69, above).
- *The chunk header's checksum,* on every allocation and free, among the hottest instructions of `allocate` and
  `deallocate`. It was the BSD checksum in software: Scudo chooses the hardware CRC at start only if
  `computeHardwareCRC32` is linked, its reference is weak, and a weak reference does not take `crc32_hw.cpp`'s object out
  of its archive (S0 to S2 have no CRC instruction). `POLAR_SCUDO_CRC32=everywhere` (`757fbe30`) compiles every source
  with the CRC flag, so the checksum is the instruction, inline.
- *Refilling and draining the thread caches:* 0.3%. A larger cache (the "MaxBytesCachedLog" option) would not help.
- *musl's `memcpy`,* about 2.6 µs a message more than glibc's, in every musl build (V2's as well). Not taken up here.

## The third set: S3 and S4, with tracing at warn (2026-10-29)

*Cleared (the maintainer, 2026-10-29):* the full baseline, two rounds. Every build from this branch with the two tracing
commits, so tracing is at `warn` with the span at `debug`, as the broker will be:

- V0, glibc (the reference); V2, musl with mimalloc (the control);
- S2, as in the second set;
- S3, S2 with the CRC instruction everywhere;
- S4, S3 with one thread cache a thread (`polar-scudo/configs/exclusive`).

S3 against S2 is the checksum; S4 against S3, the lock, and its memory. The procedure is the second set's (scratch driver
`exp-driver4.nu`), with u7c holding only what the harness needs: the broker, cert-issuer, the two credentials agents and
Neo4j (the credentials agent writes to it). Flux's `polar-agents` and `polar-data` are suspended and their workloads
scaled to 0 (ace, the git, jira, kube and scheduler agents, the git server, Jaeger, Keycloak, Postgres), at the
maintainer's request: about 12 GB of the host's memory, eleven of it custody daemons at 1.06 GiB each, before the
`MCL_ONFAULT` fix reached them. The check counts the three clients left (cert-issuer and the credentials agents), not
ten. Numbers from this set are compared only with each other.

*The machine:* the maintainer's development host is tuned for interactive use (2026-10-29), so what the broker carries
here is what it carries at the least; on production hardware it does better. The comparisons between builds hold; the
absolute figures are a floor.

## The third set: the results (2026-10-29)

Run 14:55 to 17:38, rounds in the order V0, S2, S3, S4, V2, twice; scratch `exp-driver4.nu`, `exp4-analyse.nu`,
records `baseline-20261029T145837.nuon` to `baseline-20261029T172405.nuon`. Every check passed (6 or 7 sessions, nothing
refused), the broker held through every run, and no message was lost. Two checks recorded a client's restart, as the
driver records and does not wait on (above): cert-issuer before S4's first round, the credentials agent's observer
before S3's second, each by its pod's restart-all-containers rule. u7s was stopped for the set
(the maintainer, 2026-10-29). Each cell is round 1, round 2.

| | V0 glibc | S2 | S3 | S4 | V2 mimalloc |
| --- | --- | --- | --- | --- | --- |
| 256 B saturated, received a second | 84,586, 75,375 | 60,978, 60,887 | 60,835, 63,616 | 76,906, 78,246 | 77,098, 76,364 |
| 256 B saturated, CPU a message | 51, 57 µs | 87, 81 | 83, 77 | 58, 53 | 60, 62 |
| 4 KiB saturated, received a second | 62,155, 63,607 | 53,842, 50,404 | 52,990, 52,464 | 57,407, 57,143 | 57,647, 57,602 |
| 64 KiB saturated, received a second | 19,691, 24,469 | 22,547, 22,022 | 21,742, 15,831 | 14,329, 22,617 | 14,811, 23,544 |
| 1 MiB saturated, received a second | 1,893, 1,879 | 1,781, 1,664 | 1,789, 1,560 | 1,622, 1,683 | 1,611, 1,511 |
| working set, 256 B at 10,000/s | 8, 8 MB | 10, 10 | 10, 10 | 11, 12 | 54, 54 |
| working set at the end | 72, 88 MB | 50, 38 | 40, 33 | 47, 50 | 82, 83 |
| peak after the small-message rows | 36, 34 MB | 26, 27 | 27, 26 | 28, 29 | 82, 76 |
| peak after every row | 150, 119 MB | 58, 50 | 51, 63 | 54, 77 | 122, 122 |

At the set rates every build kept up (received equals offered at every size, up to 10,000 a second), and the 1 MiB CPU
is level (0.9 to 1.2 ms a message in every build).

*The shared caches' lock was the lever.* S4, one thread cache a thread, carries as many small messages as mimalloc
(76,906 and 78,246 a second at 256 B against V2's 77,098 and 76,364; 4 KiB level too) at the CPU mimalloc spends (53 to
58 µs a message against 60 to 62), and is within V0's spread at 256 B (75,375 to 84,586) and about 9% below it at 4 KiB.
S2 and S3, whose caches are shared, carry about a fifth fewer at 256 B, at 77 to 87 µs.

*The inline CRC did nothing measurable:* S3 is S2 in every row. The checksum is hot in a profile but small beside the
rest.

*S4's memory is still Scudo's:* a peak of 54 and 77 MB against mimalloc's 122 and 122 and glibc's 150 and 119; at the
end 47 and 50 MB against 82 and 83. Under small messages it holds 11 to 15 MB, where mimalloc holds 51 to 58. One cache
a thread costs a little against S2 (the peak 54 and 77 against 58 and 50; 1 to 4 MB more under small messages), far less
than the second set's S1 against S2 suggested.

*The 64 KiB saturated row is noise here:* every build moves by up to 9,000 a second between rounds (S4 14,329 then
22,617; V2 14,811 then 23,544).

*Against the second set,* three things changed at once (the tracing commits, the cluster pared down to what the harness
needs, u7s stopped), so its figures are not compared with these. The profile's run at `warn` alone (above) puts the
most of the change on tracing.

*Not decided here:* which build the broker uses. S4 carries what V2, every binary's current build, carries, with about
half its peak memory and a quarter of its working set under small messages.

## Decided (2026-10-29)

*The maintainer:* one allocator in every binary, S4, and one artifact per target. Every Polar binary is static for musl
(`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`) and allocates with Scudo in S4's configuration; the same
binary runs in the cluster, on a Linux host and on a Mac (in the Linux VM its container runtime provides), with the
same libraries and the same allocator, built and run by the same commands everywhere. No build for one environment
only, and no fallback to another allocator where Scudo cannot be compiled: Scudo runs on Linux, and Polar's binaries are
Linux binaries.

- `polar-scudo` is every binary's global allocator (`static GLOBAL: polar_scudo::Scudo`), and the only one:
  `polar-scudo/tests/every_binary.rs` reads the workspace's binaries from `cargo metadata` and fails on one that does
  not declare it, or on a package that depends on another allocator. mimalloc and smalloc are out of the workspace.
- Scudo's source is the flake input `nix-scudo`, the maintainer's repository: LLVM's
  `compiler-rt/lib/scudo/standalone` split out with its history, at `llvmorg-23.1.0` (the source measured here),
  unmodified, with a flake whose `lib.mkScudo` builds it as a static library with the caller's configuration (its README
  says how to move it to a later LLVM). The configuration stays in Polar, `polar-scudo/config/custom_scudo_config.h`,
  S4's. `src/agents/workspace.nix` builds it for this system's musl target, which every binary ships with, and its gnu
  target, which `cargo test` builds for, at `<dir>/<target>/libpolar_scudo.a`; `POLAR_SCUDO_LIB_DIR` names the
  directory, set by the workspace's Nix build, the development shell, the development container (`/lib/polar/scudo`)
  and the CI container. `polar-scudo/build.rs` links it, and stops, saying why, when the variable is unset or the target
  has no library: every build is the same `cargo build`, linking the same library.
- Every Scudo source is compiled with the CRC instructions (SSE4.2 on x86_64, the CRC extension on aarch64), as S4 was
  (`mkScudo`'s `crc`, on by default).
- The experiment's builds (`cassini/experiment.nix`, the broker's allocator features, S0 to S3's configurations) are
  removed; they are in this branch's history up to `3cd2272f`.

## What measuring it found

Each fix was made test first, the test failing before it.

1. **The broker shut itself down when a subscription could not be made** (`2bc47be7`). `SubscriberManager` answered a
   subscription it could not make (no session, or a session manager that did not answer in 100 ms) and also failed, and
   a failed worker is fatal to the broker: six orderly shutdowns in 35 minutes of the first sweep, and every agent's
   health check then ended it. It is refused now, and the manager carries on.
2. **A topic kept its ended subscribers** (`dc06e841`). Every later message was sent to them, lost and logged, once per
   ended subscriber: in a few minutes the broker's log was 30,397 such lines. A subscriber that cannot take a message is
   dropped at once, and what reached no one is kept for the next.
3. **A subscription could go unanswered, and held up publishing** (`631fb0e1`). The topic manager, which routes every
   publish, waited up to 200 ms on the subscriber manager inside its handler, and on a timeout told the client nothing:
   a sink that never heard four of its six subscriptions was never ready. It asks in a task of its own, waits up to ten
   seconds, and always answers.
4. **ractor's output ports dropped a subscriber that fell behind, for good** (`92bc4822`). Every client's events go
   through one; a subscriber more than ten messages behind had its forwarding task end, without a word, and heard
   nothing again: producers stopped hearing acknowledgements after about a thousand, and a local test's subscriber heard
   1 of 5,000 messages. The workspace uses `output-port-v2`.
5. **Every publish formatted its payload into a span** (`dfb432cb`). The client's handler is instrumented at INFO with
   `msg = ?message`, which writes a payload out byte by byte: about 27 ms a MiB, the 14 MB/s ceiling above. The span
   records the message's kind. Locally, through the broker: 1 MiB messages from 37 MB/s to 1,011 MB/s, 256 bytes from
   64,000 to 127,000 a second.
6. **Pods of one workload share an identity, and the broker keeps one session per identity** (`1020136a`). The plan's
   four-producer, four-sink runs were four pods of one workload, each taking the others' sessions over as it registered;
   their one-second latency tails (the client's reconnection backoff), missing acknowledgements and losses were that. The
   baseline measures one connection until pods can have identities of their own.
7. **Every publish's span was exported** (2026-10-29, u7c). The listener opens an `info` span for each publish
   (`listener.rs`, `cassini.request.publish_request`) and carries it through the actors that handle the message; the
   trace filter, `info` by default, admitted it, and every span it admits is recorded and batched for export, whether
   or not a collector answers. Profiled with `perf record` on the broker's process, 256 B messages as fast as the
   window allows (1,000,000 of them, Jaeger scaled to 0): tracing was 37 to 42% of the broker's CPU, on glibc and on
   the musl branch's Scudo build alike, and no thread was busy (about 1.2 cores over seven workers; the work on a
   message is close to serial, so a message's CPU sets the rate). With `POLAR_TRACE_FILTER` at `warn`, one run each:
   glibc from 11,580 to 19,027 a second (96 to 50 µs of CPU a message), Scudo from 10,984 to 13,151 (108 to 70 µs).
   The default is `warn` for every agent now (`polar::logging::DEFAULT_TRACE_FILTER`), raised per workload when more
   is wanted. Tracing is still about a quarter of the CPU at `warn`: the console filter, `info`, still admits the span,
   so it is still created and entered in each actor.

Also fixed on the way: `polar-init`'s escrow bootstrap had no deadline, and one pod waited in its init container for
eleven minutes (`8030abdd`); the harness keeps a failed run's logs and goes on (`87a643cf`); a resources file of one
document (`c5ab1a5f`); the render's layer checks and rendered-only files (`9f759264`).

## Open

- Several connections at once: pods of one workload need identities of their own, or the broker sessions per pod
  instance. A decision, not a fix.
- The client's read loop goes on reading after a failed `read_exact`, from the middle of a frame (seen in the code, not
  in a run).
- In the earlier, invalid four-by-four runs one message in 100,000 was still lost after the fixes; with one connection
  none was. Measure again once several connections are valid.
