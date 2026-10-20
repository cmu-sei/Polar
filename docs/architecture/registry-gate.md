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

# A gate in front of the registry in the cluster

Status, 2026-10-20: a design for review. Nothing here is built.

## The problem

The registry in the cluster (`infra/cluster-registry.nu`) is the upstream `registry:2` on a NodePort, over plain HTTP,
with deletes enabled and no authentication: anyone who reaches a node at the NodePort can push, and replace any image a
Polar workload pulls by tag. Pushing is the supply-chain risk. Pulls of Polar's images are not secret (they are published
on Docker Hub, and none carries a secret), so pulls stay anonymous for now.

## What constrains it

- **Nodes pull from `localhost:<NodePort>` over plain HTTP.** That is what makes the registry work on any cluster: a node's
  container runtime pulls from a `localhost` registry over HTTP with no configuration. TLS on the pull path, from Polar's
  CA, would need every node's runtime to trust that CA, which is node configuration. So the pull path stays plain HTTP on
  the node's localhost.
- **A gate must not be one that can be gone around.** The dev CNI does not enforce NetworkPolicy, so the upstream registry
  cannot rely on one to be unreachable.
- **The registry comes before the cluster's CA and its first account.** `up.nu` applies it first (everything Polar runs is
  pulled from it), pushes every image in the images step, and makes the cluster CA after the render and the first account
  in the admin flow, near the end. So the first pushes happen when no Polar certificate exists to authenticate them.

## The shape

One pod, two containers:

- **The upstream registry**, listening on the pod's loopback only (`127.0.0.1:5000`). Nothing outside the pod can reach
  it, whatever the network does: the gate is the only way in.
- **The gate**, a small Polar agent, with two listeners:
  - **pull** (plain HTTP, the NodePort the nodes pull from): `GET` and `HEAD` of `/v2/` only, passed through. Every other
    method is refused.
  - **push** (mTLS, a ClusterIP Service, reached by port-forward from an operator's machine or the dev pod): the whole
    registry API, for an identity allowed to push. Deleting is refused to everyone.

Every accepted push of a manifest is recorded: who pushed which digest under which tag, from which address. The record is an
append-only log in the gate's volume (like the git server's `push-log.jsonl`), and an event on Cassini once Cassini runs, so
the graph can say which image a pod runs and who put it there.

## Who may push, before the cluster has a CA

The deployment git server met the same ordering: it runs before the cluster's CA, and authenticates with a PKI of its own,
the deployment PKI, made by `infra/deploy-repo.nu` on the operator's machine (`~/.config/polar-deploy/<target>/`: a CA, the
server's certificate, the pipeline's, Flux's; the CA key never goes to the cluster). The gate would use the same:

- its server certificate, and a `pusher` client certificate for the operator, from the deployment PKI, made by the registry
  step when they are not there;
- pushes allowed for the `pusher` identity, exactly, as the git server allows its administrators;
- later, once the cluster CA and accounts exist, an account holding a push capability as well (open question 2).

## Open questions

1. **The deployment PKI for the registry too.** It exists only for gitops targets today (`sandbox-local`). Making it the
   registry's as well means every target with a registry in the cluster has one: is that the right home for the registry's
   trust, or should the registry have a PKI of its own?
2. **Pushing with a Polar account.** Once accounts exist, should an account's certificate be able to push (with a new `c:`
   capability, say `c:push-image`)? Then a developer pushes as themself, and the record names them.
3. **What the gate is.** A new crate, beside `polar-git-server` (Rust, the same TLS and identity code), proxying to the
   upstream registry. It does not implement the registry API, only decides who may use it.
4. **Bootstrap pulls of the gate's own image.** The registry and its gate are pulled before the registry exists, so both
   come from Docker Hub by digest, as `registry:2` does now. The gate's image would be published there with the others.
