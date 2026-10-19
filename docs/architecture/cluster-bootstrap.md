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
| [overview](sequences/bootstrap/overview.png) | the three phases on one page |
| [phase 1: before Flux](sequences/bootstrap/phase1-before-flux.png) | `up.nu` up to installing Flux |
| [phase 2: Flux](sequences/bootstrap/phase2-flux.png) | Flux reconciling the layers |
| [phase 3: after Flux](sequences/bootstrap/phase3-after-flux.png) | the admin flow, the seed, the browser |

The sources are the `.mmd` files beside them (regenerate as in `sequences/assets/asset_regeneration.md`).

## What exists and what is proposed

**Everything not marked (new) runs today.** It was traced from the code: `infra/up.nu`, `plan.nu`, `images.nu`,
`render.nu`, `render-catalog.nu`, `resolve.nu`, `layers.nu`, `secrets.nu` of the target, `tools.nu`, `forwards.nu`, the
Justfile recipes (`registry-up`, `port-forwards-up`, `admin-bootstrap`, `admin-provision`, `admin-claim`,
`user-cert-install`), and `polar-admin-grant` and `polar-sign`. **(new)** marks the proposed Flux deployment, none of
which is built except the deployment host (`deploy-repo.nu ensure`): `deploy-repo.nu publish`, `flux.nu`, and Flux itself. Today `up.nu` applies the
manifests itself, in `apply_order`, with one gate (Cassini); the Flux target replaces that loop and nothing else.

## Why three phases

| Phase | Who | What it is | Why it is there |
|---|---|---|---|
| 1 before Flux | `up.nu` | build, render, publish, namespaces, the cluster CA, the secrets, install Flux | needs a human's machine (the SOPS key, the kubeconfig), or has to exist before any workload can start |
| 2 Flux | Flux | apply the six layers in order, each after the one before is healthy | the part that is GitOps: declared in a repository, repeatable, reverted with `git revert` |
| 3 after Flux | `up.nu` | port-forwards, the admin flow, the seed, the browser, the report | needs running workloads (the credentials agent, the git server, Neo4j), and a person |

## The steps, with the script and what each makes

### Phase 1 (`up.nu` calls these, in this order)

| # | Step | Script | Makes |
|---|---|---|---|
| 1 | preflight | `up.nu` | nothing. Checks the cluster answers, a container runtime exists, the SOPS files decrypt |
| 2 | agent images (`--dev`) | `just registry-up`, `resolve.nu document-images`, `images.nu sync`, `nix build`, `skopeo` | the registry container; `result/IMAGE` archives; images in the registry |
| 3 | render | `render.nu`, each chart's `main.nu`, `render-catalog.nu`, `resolve.nu`, `dhall` | `manifests/` (51 files), render cache |
| 4 | images (`--dev`) | `images.nu` | the images the manifests name |
| 5 | split (new) | `layers.nu split` | one directory per layer and `layers.json` |
| 5 | deployment host | `deploy-repo.nu ensure` | the deployment PKI; the deployment git server container. Built 2026-10-19 (`nu infra/deploy-repo.nu ensure <target>`, idempotent; the container is `polar-deploy-<target>` on host port 9443, its repositories in the volume `polar-deploy-<target>-repos`) |
| 5 | publish | `deploy-repo.nu publish` | a signed commit in `deploy-T`: `layers/<layer>/` (every layer but the bootstrap `flux` one), `flux/` (the GitRepository and a Kustomization per layer with `dependsOn`, `force` where the layer has Jobs, and health checks made from the layer's `wait_for`; a Job is waited for through the Endpoints of the Service that selects its pods, see "What looks odd"; a Job with no such Service is not waited for), `provenance.json` (source commit, the digest of every file, the image lock, the CAs) and a README. Built 2026-10-19. Signed with git's x509 support (`gpgsm`) using the `pipeline` certificate in a GnuPG home beside the PKI; the trust is the deployment CA only, and nothing in it comes from the cluster. Refuses an unrendered target, an image not pinned by digest, and a dirty tree (`--allow-dirty` records it). Running it again on an unchanged render commits nothing |
| 6 | namespaces | `up.nu` (`kubectl apply`) | the namespaces |
| 7 | cluster CA | `polar-admin-grant bootstrap-ca` | Secret `cert-issuer-ca`, ConfigMap `polar-ca`; straight to the API with your kubeconfig, created once and never overwritten (a second run fails with 409) |
| 8 | secrets | `targets/T/secrets.nu`, `sops`, `rage` | the Secrets listed below |
| 9 | install Flux | `flux.nu install` | Built 2026-10-19; the git half is checked, the cluster half is not yet run. First it clones the deployment repository as `flux` and checks that its head is signed by the pipeline's certificate (`git verify-commit` in the PKI's GnuPG home); an unsigned or unpublished head stops it before the cluster is touched. Then it applies the rendered controllers and waits for their CRDs and Deployments; a Service `polar-deploy-git` (the name on the server's certificate) with an EndpointSlice to the host (`--host`, `10.0.2.2` on the podman node); Secret `deploy-repo-auth` (the `flux` certificate, key and the deployment CA, never printed); and the GitRepository and Kustomizations from the verified clone |

### Phase 2 (Flux)

`platform` → `cert-issuer` → `data` → `cassini` → `identity` → `agents`, each a Kustomization that `dependsOn` the one before
and is healthy before the next starts (what it waits for is in `layers.json`, copied from what `up.nu` waits for today).
`cassini` is a layer of its own because a client that starts before its broker attaches to a broker that is about to go away.
`flux.nu wait <target>` polls until every Kustomization is Ready *and* was applied from the commit that was published (a layer that is Ready at an older commit does not count), printing a layer's state when it changes, and on timeout names the layers that are not.

### Phase 3 (`up.nu` again)

wait for the core services → port-forwards (`just port-forwards-up`) → admin flow (`just admin-bootstrap`,
`admin-provision`, `admin-claim`, all `polar-admin-grant`) → seed (`git`, `polar-sign runtime`, `git push`) →
browser (`just user-cert-install`, the Neo4j password check, both with a person) → report (`kubectl get pods`, the egress
probe). The bootstrap-admin exchange itself is drawn in `sequences/general/admin_bootstrap_identity.mmd`.

## Artifacts

| Artifact | Made by | Read by | Where | Notes |
|---|---|---|---|---|
| images | `images.nu` (nix build, skopeo copy, mirror) | the nodes, the render (to ask an agent for its scaffold), the deployment git server | registry container on `:5000` | ours are `:latest` today; Flux cannot see one change |
| `result/IMAGE` | `nix build` | `images.nu` | repo root, ignored by git | docker archives |
| `manifests/*.yaml` | `render.nu` | `up.nu` (today), `layers.nu` | repo root, ignored by git | 51 files, identical on every render (`infra/tests/render-determinism.nu`); no Secret documents |
| render cache | `render-catalog.nu` | later renders | `~/.cache/polar` | keyed on a hash of every input; never changes the output |
| layer tree, `layers.json` (new) | `layers.nu split` | `deploy-repo.nu` | a temp directory | the order, the dependencies and the waits |
| `deploy-T` repository (new) | `deploy-repo.nu publish` | Flux's source-controller; the verify step | on the deployment git server | `layers/`, `flux/`, `provenance.json`, a README that says it is generated; commits signed with the pipeline's certificate |
| deployment PKI (new) | `deploy-repo.nu ensure` | the git server, the pipeline, Flux | `~/.config/polar-deploy/T/` | its own CA, a server certificate, and client certificates for `pipeline` (writes) and `flux` (reads); not Polar's CA |
| `push-log.jsonl` (new) | the deployment git server | whoever audits | beside its repositories | who pushed what, when |
| Secret `cert-issuer-ca`, ConfigMap `polar-ca` | `polar-admin-grant bootstrap-ca` | cert-issuer; everyone trusting the CA | the cluster | the CA's key lives only in the cluster |
| Secrets `neo4j-secret`, `polar-graph-pw`, `cassini-shutdown-token`, `jira-secret`, `oci-registry-auth`, `keycloak-admin-secret`, `keycloak-db-secret` | `secrets.nu` | the workloads | the cluster | from `secrets.sops.yaml` and `user.sops.yaml`; never in the render or the deployment repo |
| Secret `flux-system/deploy-repo-auth` (new) | `up.nu` | source-controller | the cluster | `tls.crt`, `tls.key`, `ca.crt` of the `flux` identity |
| Flux controllers, GitRepository, Kustomizations (new) | `flux.nu install`, then Flux | Flux | the cluster | the Kustomizations are also in the repository, under `flux/` |
| `~/.config/polar-admin/` | `just admin-*`, `polar-admin-grant` | `polar-sign`, `git`, the browser step | the home directory | `ca.pem`, `bootstrap-admin.*`, `U.crt`, `U.key`, `U.chain.pem`; moved aside to `polar-admin.old-TIMESTAMP` when its CA is another cluster's, never deleted |
| signed documents in `polar-schedules` | `polar-sign runtime` | the scheduler observer, then the agents | the in-cluster git server | `permanent/AGENT/SECTION.dhall`: runtime configuration, a separate repository from `deploy-T` and a separate trust path |
| certificate store entries | `just user-cert-install` | the browser | the Librewolf profile | the CA and U's PKCS12 |
| `POLAR_GIT_SERVER_ADMINS` in `targets/T/config.nuon` | the operator | `render.nu` | the source repository | a value that belongs to one cluster; see below |

## What looks odd, and why

- **The git server's administrator is a per-cluster value, and `up.nu` writes it.** The identity that may seed
  `polar-schedules` is U's certificate, which exists only after the admin flow, but the git server's configuration
  needs it. The seed step reads the first DNS name of U's certificate, adds it to `POLAR_GIT_SERVER_ADMINS` in
  `targets/T/config.nuon` (`seed.nu` `with_admin`), and renders and applies again if that changed anything; under Flux
  that is a new commit that Flux applies before the seed resumes. The value in a committed `config.nuon` is whatever the
  last cluster left, and is not meant to be edited.
- **Namespaces are applied by `up.nu` and again by Flux.** The CA and the secrets need the namespaces before Flux runs, and the
  `platform` layer also declares them. Flux adopts what `up.nu` applied; the content is the same.
- **`up.nu` creates secrets that Flux does not own.** They are not in the repository, so Flux never prunes them, and they
  have to exist before the workloads that use them start. That is intended.
- **A Job never completes, and Flux cannot be told a running Job is healthy.** Every agent is a Job that runs until it is replaced (about hourly, before its certificate expires), so `wait: true` cannot be used on the `agents` layer. A health check on the Job itself does not work either: Flux's kustomize-controller registers its own Job status reader ahead of any `healthCheckExprs`, and it counts a Job as Current only when it has completed (found 2026-10-19: `polar-cassini` stayed "Running health checks" for ten minutes with a Ready pod, and a CEL expression on the Job was never consulted; `fluxcd/pkg` `runtime/statusreaders/job.go`). What says an agent is up is the Service that selects its pods: its `Endpoints` has a deterministic name and no `subsets` until a pod is ready. `publish` gives a layer a health check on that Endpoints with `subsets.exists(s, has(s.addresses) && size(s.addresses) > 0)` (`has()` takes only a field selection, so the missing field is left to make Flux wait). `cassini` and `identity` are gated this way (the broker and the credentials observer). The credentials agent Job has no Service, so nothing waits for it separately; its observer depends on it. A layer's `wait_for` of a Job with no Service is dropped.