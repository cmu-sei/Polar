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
