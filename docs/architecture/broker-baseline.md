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
