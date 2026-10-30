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

# Bootstrapping a cluster

How a cluster goes from nothing to running, for the `sandbox-local` target: every script that runs, in
order, and every artifact it makes. The diagrams are in `sequences/bootstrap/`:

| Diagram | Covers |
|---|---|
| [overview](sequences/bootstrap/overview.svg) | the three phases on one page |
| [phase 1: before Flux](sequences/bootstrap/phase1-before-flux.svg) | `up.nu` up to installing Flux |
| [phase 2: Flux](sequences/bootstrap/phase2-flux.svg) | Flux reconciling the layers |
| [phase 3: after Flux](sequences/bootstrap/phase3-after-flux.svg) | the admin flow, the seed, the browser |

The sources are the `.mmd` files beside them (regenerate as in `sequences/assets/asset_regeneration.md`).

## What exists

**Everything described here runs.** It was traced from the code (`infra/up.nu`, `plan.nu`, `images.nu`, `render.nu`,
`render-catalog.nu`, `resolve.nu`, `layers.nu`, `registry.nu`, `deploy-repo.nu`, `deploy-objects.nu`, `flux.nu`,
`cluster-inputs.nu`, `secrets.nu` of the target, `tools.nu`, `forwards.nu`, the Justfile recipes `registry-host-up`, `port-forwards-up`,
`admin-bootstrap`, `admin-provision`, `admin-claim` and `user-cert-install`, and `polar-admin-grant` and `polar-sign`), and a clean
`sandbox-local` bring-up was run end to end on 2026-10-19 on the podman dev node (all six layers Ready, the seed applied). Other
runtimes (minikube, kind, a managed cluster) have not run the gitops target; `local-minimal` does not use Flux and `up.nu` applies it
itself, in `apply_order`, with one gate (Cassini). A target opts in with `gitops: true` (`plan.nu` swaps the apply loop for steps 8 to 11
below).

## Why three phases

| Phase | Who | What it is | Why it is there |
|---|---|---|---|
| 1 before Flux | `up.nu` | build, render, namespaces, the cluster CA, the secrets, the deployment git server, publish, install Flux | needs a human's machine (the SOPS key, the kubeconfig), or has to exist before any workload can start |
| 2 Flux | Flux | apply the layers in order, each after the one before is healthy | the part that is GitOps: declared in a repository, repeatable, reverted with `git revert` |
| 3 after Flux | `up.nu` | port-forwards, the admin flow, the seed, the browser, the report | needs running workloads (the credentials agent, the git server, Neo4j), and a person |

## The steps, with the script and what each makes

### Phase 1 (`up.nu` calls these, in this order)

| # | Step | Script | Makes |
|---|---|---|---|
| 1 | preflight | `up.nu` | nothing. Checks the cluster answers, runs Kubernetes 1.36 or later, and namespaces can be made, this machine's `openssl` is OpenSSL 1.1.1 or later (not macOS's LibreSSL; `infra/preflight.nu`), a container runtime exists when a step starts a container here (`--dev`, the deployment host), the SOPS files decrypt; deletes scratch namespaces an interrupted run left behind |
| 1b | registry (`cluster.registry_in_cluster`) | `cluster-registry.nu ensure` | for a target whose registry is in its cluster: the storage chart (provisioner from upstream), namespace `polar-registry` with the registry (upstream image by digest, a claim on the target's class, a NodePort the nodes pull from at `localhost:<port>`), and the port-forward this machine pushes through. Applied by `up.nu`, never by Flux: everything Flux runs, its controllers included, is pulled from it |
| 1c | import (`--import-from`) | `images.nu import` | the published images, Neo4j included, copied into the registry |
| 2 | agent images (`--dev`) | `just registry-host-up`, `resolve.nu document-images`, `images.nu sync`, `nix build`, `skopeo` | the registry container; `result/IMAGE` archives; images in the registry |
| 3 | render | `resolve.nu collect-scaffolds` (`agent-pods.nu`), `render.nu`, each chart's `main.nu`, `render-catalog.nu`, `resolve.nu`, `dhall` | the agents' scaffolds, asked of their images as pods in a scratch namespace (deleted afterwards); `manifests/` (52 files), the render caches |
| 4 | images (`--dev`) | `images.nu` | the images the manifests name |
| 5 | namespaces | `up.nu` (`kubectl apply`) | the namespaces |
| 6 | cluster CA | `polar-admin-grant bootstrap-ca` | Secret `cert-issuer-ca`, ConfigMap `polar-ca`; straight to the API with your kubeconfig, created once and never overwritten (a second run fails with 409) |
| 7 | secrets | `targets/T/secrets.nu`, `sops`, `rage` | the Secrets listed below; the same script `up.nu` runs for a target it applies itself |
| 8 | deployment git server | `deploy-repo.nu ensure` | the deployment PKI under `~/.config/polar-deploy/T/` (its own CA, a server certificate, and client certificates for `pipeline`, which writes, and `flux`, which reads; not Polar's CA, which does not exist before Polar does) and the server, in the cluster: namespace `polar-deploy`, held by no layer, so Flux neither applies nor prunes it; a Deployment of `polar-git-server` (by digest, from the node registry), its repositories on a claim, a Secret with the server's certificate and key and the CA certificate (the CA key never leaves this machine), and the Service `polar-deploy-git` on 9443, `polar-deploy-git.polar-deploy.svc.<domain>` on the server's certificate. This machine reaches it through `just port-forwards-up deploy` (localhost:9443). A new, empty server is given the history of `mirror.git` beside the PKI, which `publish` updates. Moved into the cluster 2026-10-19 (it was a container on the host, reached by the nodes through an EndpointSlice to the host's address) |
| 9 | publish | `deploy-repo.nu publish` | one signed commit in `deploy-T`: `layers/<layer>/` (every layer but the bootstrap `flux` one, with a `polar.dev/config-hash` fingerprint of the ConfigMaps a workload reads in its pod template), `flux/` (a GitRepository and a Kustomization per layer: `dependsOn`, `force` where the layer has Jobs, and health checks made from the layer's `wait_for`, see "What looks odd"), `provenance.json` (the source commit, the digest of every file as rendered and as published, the image lock, the CAs, and `cluster_inputs`) and a README. Signed with git's x509 support (`gpgsm`) using the `pipeline` certificate in a GnuPG home beside the PKI; the trust is the deployment CA only, and nothing in it comes from the cluster. Refuses a target that is not rendered, an image not pinned by digest, and a dirty tree (`--allow-dirty` records it); on an unchanged render it commits nothing |
| 10 | install Flux | `flux.nu install` | First it clones the deployment repository as `flux` and checks that its head is signed by the pipeline's certificate (`git verify-commit` in the PKI's GnuPG home): an unsigned or unpublished head stops it before the cluster is touched. Then it applies the rendered controllers and waits for their CRDs and Deployments; Secret `deploy-repo-auth` (the `flux` certificate, key and the deployment CA, never printed); the GitRepository, waiting until Flux has fetched the repository; and the Kustomizations, from the verified clone |
| 11 | wait for Flux | `flux.nu wait` | nothing; it follows the layers (phase 2) until every Kustomization is Ready at the published commit |

### Phase 2 (Flux)

`platform` → `cert-issuer` → `data` → `cassini` → `identity` → `agents`, each a Kustomization that `dependsOn` the one before
and is healthy before the next starts. What it waits for is each layer's `wait_for` in the target, turned into health checks (a StatefulSet or Deployment as itself, an agent's Job through the Endpoints of its Service); the `data` layer waits for nothing, because Neo4j cannot be Ready until the credentials agent of a later layer is up.
`cassini` is a layer of its own because a client that starts before its broker attaches to a broker that is about to go away.
`flux.nu wait <target>` polls until every Kustomization is Ready *and* was applied from the commit that was published (a layer that is Ready at an older commit does not count), printing a layer's state when it changes, and on timeout names the layers that are not. A layer waiting on a health check begun under an older spec or revision is ended by restarting the kustomize-controller (see "What looks odd").

### Phase 3 (`up.nu` again)

wait for the core services → port-forwards (`just port-forwards-up`) → admin flow (`just admin-bootstrap`,
`admin-provision`, `admin-claim`, all `polar-admin-grant`) → seed (records the first admin's identity in `cluster-inputs.nuon`, renders and publishes again, waits for Flux to restart the git server, then `git`, `polar-sign runtime`, `git push`) →
browser (`just user-cert-install`, the Neo4j password check, both with a person) → report (`kubectl get pods`, the egress
probe). The bootstrap-admin exchange itself is drawn in `sequences/general/admin_bootstrap_identity.mmd`.

## Artifacts

| Artifact | Made by | Read by | Where | Notes |
|---|---|---|---|---|
| images | `images.nu` (nix build, skopeo copy, mirror, import) | the nodes, the render (to ask an agent for its scaffold) | registry container on `:5000` | a gitops target names them by digest from the image lock, so Flux sees a change; the others use `:latest`, pulled `Always` |
| `result/IMAGE` | `nix build` | `images.nu` | repo root, ignored by git | docker archives |
| `manifests/*.yaml` | `render.nu` | `up.nu` (a target it applies itself), `deploy-repo.nu publish` | repo root, ignored by git | 51 files (52 for `sandbox-local`), identical on every render (`infra/tests/render-determinism.nu`); no Secret documents |
| render cache | `render-catalog.nu`, `dhall-render.nu`, `resolve.nu` | later renders | `~/.cache/polar` | the catalog's manifests, the hand-written charts' manifests and the agents' scaffolds, each keyed on a hash of every input; never changes the output |
| `deploy-T` repository | `deploy-repo.nu publish` | Flux's source-controller; the verify step | on the deployment git server | `layers/`, `flux/`, `provenance.json`, a README that says it is generated; commits signed with the pipeline's certificate |
| deployment PKI | `deploy-repo.nu ensure` | the git server, the pipeline, Flux | `~/.config/polar-deploy/T/` | its own CA, a server certificate, and client certificates for `pipeline` (writes) and `flux` (reads); not Polar's CA |
| `push-log.jsonl` | the deployment git server | whoever audits | beside its repositories | who pushed what, when |
| Secret `cert-issuer-ca`, ConfigMap `polar-ca` | `polar-admin-grant bootstrap-ca` | cert-issuer; everyone trusting the CA | the cluster | the CA's key lives only in the cluster |
| Secrets `neo4j-secret`, `polar-graph-pw`, `cassini-shutdown-token`, `jira-secret`, `oci-registry-auth`, `keycloak-admin-secret`, `keycloak-db-secret` | `secrets.nu` | the workloads | the cluster | from `secrets.sops.yaml` and `user.sops.yaml`, and updated when they change, except `cassini-shutdown-token`, made in the cluster once (`infra/INFRA.md`, "Secrets"); never in the render or the deployment repo |
| Secret `flux-system/deploy-repo-auth` | `flux.nu install` | source-controller | the cluster | `tls.crt`, `tls.key`, `ca.crt` of the `flux` identity |
| Flux controllers, GitRepository, Kustomizations | `flux.nu install`, then Flux | Flux | the cluster | the Kustomizations are also in the repository, under `flux/` |
| `~/.config/polar-admin/` | `just admin-*`, `polar-admin-grant` | `polar-sign`, `git`, the browser step | the home directory | `ca.pem`, `bootstrap-admin.*`, `U.crt`, `U.key`, `U.chain.pem`; moved aside to `polar-admin.old-TIMESTAMP` when its CA is another cluster's, never deleted |
| signed documents in `polar-schedules` | `polar-sign runtime` | the scheduler observer, then the agents | the in-cluster git server | `permanent/AGENT/SECTION.dhall`: runtime configuration, a separate repository from `deploy-T` and a separate trust path |
| certificate store entries | `just user-cert-install` | the browser | the Librewolf profile | the CA and U's PKCS12 |
| `cluster-inputs.nuon` (`POLAR_GIT_SERVER_ADMINS`) | `up.nu`, at the seed step | `render.nu`, then recorded in `provenance.json` | `targets/T/`, gitignored | what belongs to one cluster and exists only after it does; see below |

## What looks odd, and why

- **The git server's administrator is a per-cluster input, rendered twice on purpose.** The identity that may seed `polar-schedules` is
  U's certificate, which exists only after the admin flow, but the git server's configuration needs it. It is an input like any other, and
  the provenance trail has to show it, so the seed step writes it to `targets/T/cluster-inputs.nuon` (`infra/cluster-inputs.nu`: gitignored,
  tied to the cluster by the sha256 of its CA, discarded when that differs), renders again and publishes a second signed commit. That commit's
  `provenance.json` names the file, its digest, the cluster and the values (`cluster_inputs`), and the tree is not dirty. Flux restarts the git
  server because the fingerprint of its ConfigMap, in its pod template, changed. The inputs replace an entry of the same name in the target's
  `config.nuon`, so a stale committed value cannot survive.
- **A reconcile finishes under the spec it began with, so a changed spec or a new revision waits behind a health check.** A Kustomization
  that is in "Running health checks" does not read a newer spec (or the next published commit) until the wait ends, up to its 10 minute
  timeout, and the condition records the generation and the revision it began under. `flux.nu wait` compares them with the current ones, and a
  wait that has been behind for 15 seconds is ended by restarting the kustomize-controller (state is in the objects, so nothing is lost; at most three
  restarts per `wait`). Reproduced and fixed on a cluster 2026-10-19: a layer given an unsatisfiable check and then corrected recovered in 57 s.
- **Namespaces are applied by `up.nu` and again by Flux.** The CA and the secrets need the namespaces before Flux runs, and the
  `platform` layer also declares them. Flux adopts what `up.nu` applied; the content is the same.
- **`up.nu` creates secrets that Flux does not own.** They are not in the repository, so Flux never prunes them, and they
  have to exist before the workloads that use them start. That is intended.
- **A Job never completes, and Flux cannot be told a running Job is healthy.** Every agent is a Job that runs until it is replaced (about hourly, before its certificate expires), so `wait: true` cannot be used on the `agents` layer. A health check on the Job itself does not work either: Flux's kustomize-controller registers its own Job status reader ahead of any `healthCheckExprs`, and it counts a Job as Current only when it has completed (found 2026-10-19: `polar-cassini` stayed "Running health checks" for ten minutes with a Ready pod, and a CEL expression on the Job was never consulted; `fluxcd/pkg` `runtime/statusreaders/job.go`). What says an agent is up is the Service that selects its pods: its `Endpoints` has a deterministic name and no `subsets` until a pod is ready. `publish` gives a layer a health check on that Endpoints with `subsets.exists(s, has(s.addresses) && size(s.addresses) > 0)` (`has()` takes only a field selection, so the missing field is left to make Flux wait). `cassini` and `identity` are gated this way (the broker and the credentials observer). The credentials agent Job has no Service, so nothing waits for it separately; its observer depends on it. A layer's `wait_for` of a Job with no Service is dropped.

## Not settled, to decide before building

1. ~~Where the Flux controllers come from.~~ Decided: rendered into the repository. `nu infra/flux.nu export` generates `flux-controllers.yaml` from the pinned `fluxcd` (2.9.6, controllers v1.9.6), the `flux` chart copies it with the images pointed at the node registry, and two mirror rows in `images.nuon` bring the images in. It is a layer of `sandbox-local`, applied for now by `up.nu` like every other layer.
2. ~~How the nodes reach the deployment git server on the host.~~ (Moot since 2026-10-19: the server runs in the cluster and Flux reaches it by its Service.) Checked 2026-10-19 on u7s: the node (`podman exec u7s curl`) reached a host container's published port at `10.0.2.2:PORT` and the server answered `200` to the `flux` client certificate (the registry is reached the same way, `10.0.2.2:5000`). The server certificate's name must be the name Flux is given; inside the cluster that is a Service with a manual Endpoints to the host address, so the name is cluster-local and no host resolution is needed. Other runtimes: not checked.
3. ~~The git server running as a container outside a cluster.~~ (Moot since 2026-10-19: it runs in the cluster.) Checked 2026-10-19: the `polar-git-server` image from the registry, run by podman on the host with a throwaway PKI, served a clone and a push for a writer, refused a push from a reader (`403`), dropped a client without a certificate in the handshake, and wrote `push-log.jsonl`. Three things it needs that a pod gets from its Job: `POLAR_HEALTH_CERTS` pointed at the real server certificate (the image's default path does not exist, and the server exits 1 after 30 s without it); a writable `/tmp` (`--tmpfs /tmp`: the health actor writes its state there, and after three failed writes, about 90 s, the server shuts itself down. The first check of this missed it by watching for less than 90 s); and a restart policy (`--restart=always`), because an agent exits by design when its certificate nears expiry.
4. Verifying a signature on push (the server rejects an unsigned or invalidly signed commit to `deploy-T`): not built. Flux's own `GitRepository.spec.verify` checks OpenPGP signatures only, not x509, so neither Flux nor the server verifies the pipeline's commits today; `git verify-commit` with the PKI's GnuPG home does.
5. ~~Pinning images.~~ Built for `sandbox-local`: `infra/lock.nu`, `infra/images.lock.nuon` and `images.dev.lock.nuon`, `images.nu lock refresh|show|promote`, and the render's `pin_images` pass. All 44 image lines of its render carry a digest, and a node pulled one by digest from the plain-HTTP registry. The committed lock does not exist yet: a release writes it from one coherent build (the dev registry holds agents built at different commits).
6. ~~Restarting a Job on a rebuilt image or a changed ConfigMap.~~ A rebuilt image changes the digest in the pod template, so Flux sees it. A changed ConfigMap: `publish` adds `polar.dev/config-hash`, a fingerprint of the ConfigMaps a Job, Deployment or StatefulSet reads, to its pod template (18 of `sandbox-local`'s 52 files), so the spec changes and `force: true` recreates the Job. Checked on a cluster (2026-10-19): after the seed added the administrator, Flux replaced the git server's pod and it read the new value.
7. ~~Whether `POLAR_GIT_SERVER_ADMINS` is committed.~~ No: it is a gitignored cluster input (`targets/T/cluster-inputs.nuon`), rendered in and recorded in the provenance (see "What looks odd").

## What was not checked

A clean `sandbox-local` bring-up ran end to end on 2026-10-19 (all six layers Ready, the seed applied), on the podman dev node. Not
exercised: the server rejecting an unsigned push (open item 4), `local` and `local-minimal` as gitops targets, and a runtime other than the
podman node. The deployment git server in the cluster, which replaced the host container and its `--host`, ran on a fresh cluster on 2026-10-19 with the registry in the cluster too: all six layers Ready at both of the bring-up's commits. For the steps that existed before Flux, I did not confirm which of the
`data` workloads get their certificate from `cert-issuer` directly and which from the credentials agent (the diagram says only that their init
containers run `polar-init`), nor where the port-forward logs are written. The node's image cache is dropped by
`podman exec <node> ctr images rm` on the host for each of the cluster's node containers (`u7s`, or each `<cluster>-<node>` of a
multi-node `u7c`; since 2026-10-27), only when `POLAR_CLUSTER_RUNTIME` is `u7s`; images that move are pulled `Always`, so it frees disk and
nothing more, and another cluster needs no such step.
