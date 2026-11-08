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

# Polar

<p align="center">
  <img width="300" height="300" src="docs/Polar-Logo.png">
</p>

Polar is a knowledge graph framework for DevSecOps observability. A set of agents observe source systems (Jira, GitLab,
Kubernetes, git repositories, OpenAPI services, and others) and write what they see into Neo4j, over Cassini, a pub/sub
broker where every connection is mutual TLS. Around the agents, Polar provides the certificate infrastructure
(`cert-issuer`, the credentials agent, Keycloak for people), the agents' builder and lifecycle, and the
Nix/Dhall/Kubernetes build and deployment pipeline these install steps use.

"Polar" is a play on Mercator, the Lending Club agents that collected infrastructure data into a graph; Polar takes the
idea further with pub/sub, mutual TLS, and external observation of services and infrastructure. See
[DOCUMENTATION.md](DOCUMENTATION.md) for the documentation map and
[Polar: Improving DevSecOps Observability](https://insights.sei.cmu.edu/blog/polar-improving-devsecops-observability/)
for an overview.

## Installation

You deploy Polar to a Kubernetes cluster you bring: one on your own machine or one somewhere else. This
section is the whole path, in the order you do it, from a cluster to a running deployment you can use and develop. Each
step says what it does, what you should see, and where you have a choice. The commands are the ones for the setup the
maintainers use (a local single-node cluster, the dev container in the cluster, building from source); the variations
follow each step, and the reference sections after the steps go into detail.

> **On a Mac?** Go to **[On a Mac, with minikube](#on-a-mac-with-minikube)**: what your Mac needs, how to set up Docker
> Desktop, and the same steps in order, for a Mac and the published images.

### At a glance

```bash
# On your machine: kubectl, ssh (with your key in its agent), git, a browser.
export KUBECONFIG=<your cluster's kubeconfig>
kubectl get storageclass                          # step 1: a cluster with storage
bin/polar-dev up                                  # step 3: your dev container, in the cluster; you land in it

# In the dev container, from /workspace:
nu infra/up.nu local-minimal --dev --user <you> --email <your email>      # step 4: Polar
# when it pauses at the browser step, on your machine:
bin/polar-dev cert <you>                          # step 5: your certificate into the browser
bin/polar-dev forward                             # step 6: then https://localhost:7473
```

### Decide first

| Decision | Choices | The examples use |
|---|---|---|
| Your cluster | A local dev node ([nix-usernetes](https://github.com/daveman1010220/nix-usernetes)), minikube (on a Mac, say: **On minikube**, below), or any cluster you can reach with `kubectl` and have admin rights in; **Kubernetes 1.36 or later** in every case | a local nix-usernetes node |
| Where you work | **The dev container in the cluster** (`bin/polar-dev`: your machine needs only `kubectl`, `ssh`, `git` and a browser), the dev container on your machine (needs `podman` or `docker`), or the tools installed on your machine. See **Working environments** | in the cluster |
| Images | **Built from source** (`up.nu --dev`; the way when you change code), or **published** ones copied in (`up.nu --import-from docker.io/<account>`; nothing is built) | built from source |
| Target | **`local-minimal`**, applied by `up.nu` directly, for one node or many; or **`sandbox-local`**, published signed to a deployment git server in the cluster and applied by Flux, its workloads placed on node pools the cluster must have (**What Polar asks of the cluster**); or a target of your own (**Using your own target**) | `local-minimal` |
| Registry | **In the cluster** (the shipped targets keep it there; nothing to run anywhere), or one you bring. See **Registries** | in the cluster |

### 1. A cluster, with storage

Point `kubectl` at your cluster and check it answers:

```bash
export KUBECONFIG=<path to your kubeconfig>
kubectl get nodes                  # at least one node, Ready
kubectl get storageclass           # at least one
```

*On a local nix-usernetes node*, a fresh cluster is:

```bash
cd ~/Documents/projects/nix-usernetes
just reset && just up && just init && just kubeconfig      # its banner's cert-manager line is out of date: skip it
export KUBECONFIG=$PWD/kubeconfig
```

*A cluster with no storage class* (a bare node, like the one above) needs a provisioner before anything else, since the dev
container's volumes come first. Polar's own storage chart is upstream's local-path provisioner, so start with upstream's:

```bash
kubectl apply -f https://raw.githubusercontent.com/rancher/local-path-provisioner/v0.0.35/deploy/local-path-storage.yaml
kubectl get storageclass           # local-path
```

*A cluster rebuilt at the same address as before* (the local node is always `127.0.0.1:6443`): clear kubectl's cache of the
old one's resource types, or `kubectl get all` asks for types it no longer has (Flux's, after a Flux target):

```bash
rm -rf ~/.kube/cache/discovery/127.0.0.1_6443
```

*On minikube* (an Apple-silicon Mac, for example), `local-minimal` is the target: one node, no Flux, no node pools. Use
minikube **v1.39.0 or later**, the first to offer Kubernetes 1.36 and 1.37 (it starts 1.37 by default; this is from its
release notes as an aggregator reports them, so check minikube's own release page). Give it as much memory and as many CPUs as
you can spare: the deployment is twenty-odd pods (Neo4j, Keycloak and Postgres among them).

```bash
minikube start --cpus <n> --memory <MiB> --extra-config=kubelet.registry-pull-qps=0
kubectl version                    # Server Version v1.36 or later
```

Then the images: on an Apple-silicon Mac they are the `arm64` ones, which `--import-from` picks by itself (step 4); the dev
container (`bin/polar-dev`) picks its own the same way. `sandbox-local` on minikube needs one node per pool, labelled and tainted
as the last item below says (`minikube start --nodes 4`, then `kubectl label` and `kubectl taint` three of them); that has not
been tried.

What Polar asks of the cluster:

- **Kubernetes 1.36 or later.** Every workload is a Deployment of one pod that restarts in place, its init containers and
  its custody sidecar with it, by a container restart rule (`RestartAllContainers`), which is on by default from 1.36
  (`docs/architecture/deployments.md`). On an older cluster its pods are refused.
- **For `sandbox-local`, its node pools.** It places each workload on a node by pool (`docs/architecture/placement.md`): a
  node each for `core`, `observers` and `writers`, labelled `polar.dev/pool=<pool>` and tainted
  `polar.dev/pool=<pool>:NoSchedule`, and at least one node without a taint for the rest. A cluster without them leaves its
  pods Pending; `local-minimal` places nothing.
- **A storage class** for persistent volumes. The `local-minimal` and `sandbox-local` targets install a local-path
  provisioner and a class named `managed-csi` (`storage.class` in `infra/config/cluster.nuon`) unless the cluster already
  has a class of that name; on a cluster with a class of its own, set `storage.class` to it.
- **A kube-proxy that answers NodePorts on `localhost`** (the iptables mode does; IPVS and some kube-proxy replacements do
  not), for the registry in the cluster (**Registries**), or a registry you bring.
- **The cluster's DNS domain** in `domain` in the same file, if it is not `cluster.local`.
- **Pods that can resolve names and reach the internet**, if you want to observe repositories on GitHub or GitLab. `up.nu`
  checks this at the end and says what it found. A DNS search domain on the node that makes every lookup slow (some
  university and corporate networks, and a VPN) is a common cause of slow or timed-out pods: a lookup of a name inside the
  cluster tries the node's search domains before the right name, and if your resolver never answers them, an agent gives up
  (its connect attempt is 5 seconds) before the answer comes. Remove the search domain from the node's resolver.
- **A kubelet that does not throttle registry requests, or one you can live with.** Polar pulls an image named by a tag
  that moves (`latest`) with `imagePullPolicy: Always`, which asks your registry for every container that starts. The
  kubelet limits that to 5 a second with a burst of 10 (`registryPullQPS`), so a burst of pods (a bring-up) sees
  `ErrImagePull: pull QPS exceeded`, backs off and retries: slower, not broken. If you can, raise it on the nodes
  (minikube: `minikube start --extra-config=kubelet.registry-pull-qps=0`).

### 2. Access to the target's secrets

The target's secrets (the Neo4j password, tokens for the systems Polar observes) are in a SOPS file encrypted to the SSH
keys of the people who may deploy it. Yours has to be one of them: send your SSH *public* key to someone who can already
decrypt the file, and once they have added it, pull. Then check, with the private key you were added with, here if you
have `sops`, or in the dev container after step 3 (where the key is already set, so `sops --decrypt` alone):

```bash
SOPS_AGE_SSH_PRIVATE_KEY_FILE=~/.ssh/<your key> sops --decrypt infra/targets/local-minimal/secrets.sops.yaml >/dev/null && echo ok
```

The details, what to do when nobody can add you, and your own `user.sops.yaml` for your personal tokens are in **Secrets
setup**, below. The dev container is given this key as `POLAR_DEV_SOPS_KEY` (step 3).

### 3. Your working environment

From the repository on your machine (a clone is all it needs to be), make your dev container in the cluster and log in to
it:

```bash
export POLAR_DEV_SOPS_KEY=~/.ssh/<your key>          # the key of step 2 (default: ~/.ssh/id_ed25519)
export POLAR_DEV_STORAGE_CLASS=local-path            # only on a cluster with no default storage class
bin/polar-dev up
```

The first time takes several minutes: it pulls the dev container image (about 2.7 GB) and copies its Nix store to a volume
(about 9.7 GB). It says what it is doing as it goes, then clones the sources into `/workspace` with your forwarded SSH
agent (`POLAR_DEV_REPO`, `POLAR_DEV_BRANCH`) and logs you in: the dev container's banner, a fish shell, `/workspace`. Inside
it, `kubectl` reaches the cluster as the pod's own account and `sops` decrypts with your key. Later logins are
`bin/polar-dev attach`. What it makes and every setting are in **Working environments**.

*Variations:* the dev container on your own machine, with `podman` (**Dev container on your machine**), or the tools on
your own machine (**Tools on your machine**). Steps 4 to 7 are the same from there, but for step 5, which on your own
machine happens by itself.

### 4. Bring Polar up

In the dev container, from `/workspace`:

```bash
nu infra/up.nu local-minimal --dev --user <name> --email <email>
```

`up.nu` prints its plan first, every step with the reason any is skipped, and then runs it: the registry in the cluster
(and the storage it needs, on a cluster that has no class yet), the images, the agents' own answers about themselves
(asked of their images as pods in a scratch namespace that is deleted afterwards), the render, the namespaces, the cluster
CA, the secrets, every manifest in the target's order, a wait for the core services, the port-forwards it needs, the admin
flow (your first account), the seed of the schedules repository (its first signed policy, and on a fresh cluster
jira-observer's credential, sealed once from the `jira_token` in the target's sops files; after that you change it with
`polar-sign` and a signed push, see **Secrets setup**) and your browser certificate. It
stops for a person only where one is needed. After a failure or a pause, **run the same command again**: every step checks
whether it is already done, and picks up where it stopped.

*Images, the choice:*

- **`--dev`** builds every image from this checkout and pushes it, through a port-forward, to the registry in the cluster.
  It is the way when you have changed code, and it rolls out every Deployment whose image it rebuilt. The first build takes a
  long while; the Nix store it fills is kept. Neo4j is built in the sibling repository **`nix-neo4j`**, not here: unless
  you built it, it is copied from where it is published (`docker.io/daveman1010220`, its row in `infra/images.nuon`). To
  use your own build, check `nix-neo4j` out at `~/Documents/projects/nix-neo4j` and build it there (`nix build -L
  .#neo4j-image -o result-neo4j-image`), or set `POLAR_NEO4J_IMAGE` to the archive. That image sets the `neo4j` user's
  password from `NEO4J_AUTH` on its first start, so there is no password to change by hand.
- **`--import-from docker.io/<account>`** copies the published images the target runs, Neo4j included, for this
  machine's architecture into the registry: nothing is built (**Loading prebuilt images**).

Either way it is one step, before the render: `up.nu` works out every image the target runs (its charts, rendered alone,
and its catalog name them), then builds or copies each once. A build's log is shown only if it fails.
- **Neither:** the images are already in the registry. Use it to resume, or to finish a run that stopped at a person's
  step; it leaves the running pods alone.

*Target, the choice:* `sandbox-local` deploys with Flux instead:

```bash
nu infra/up.nu sandbox-local --dev --user <name> --email <email>
```

It does not apply its manifests: it publishes them, signed, to a deployment git server that runs in the cluster in a
namespace of its own (`polar-deploy`, held by no layer, so Flux neither applies nor prunes it), installs Flux and waits
while Flux applies them layer by layer. Signing needs `gpgsm` (in the dev container). The deployment PKI is in
`~/.config/polar-deploy/<target>/` (its CA key never goes to the cluster) with a mirror of the signed history,
`mirror.git`, which a new cluster's server is given. Add `--allow-dirty` if the checkout has uncommitted changes (the
signed record says so). `infra/INFRA.md` ("How a target works") explains what it does. The two targets use the same
namespaces, so they do not share a cluster: rebuild the cluster between them (step 7).

What `local-minimal` deploys: Neo4j, Cassini, `cert-issuer`, the credentials agent and its observer, the ingestion
agents, a git server for the schedules repository, and Keycloak with its own Postgres (an optional identity-provider route
for certificates for people). Each runs as a Deployment of one pod that restarts in place whenever it ends: its init
container asks for its certificates again each time, which is how it rejuvenates and how it recovers when a certificate or
its environment changes.

The cluster CA is made once, by `polar-admin-grant bootstrap-ca` (the `cert-issuer-ca` Secret and the `polar-ca`
ConfigMap), and refuses to be made again; replacing it means rebuilding the cluster. Everything else gets its certificates
from it. The git server takes its administrators by identity, and an identity belongs to one cluster, so after the admin
flow `up.nu` records yours in `infra/targets/<target>/cluster-inputs.nuon` (not committed; a new cluster discards it),
renders again, and the git server restarts with it before the seed. Nothing to edit by hand. To render without applying:
`nu infra/resolve.nu collect-scaffolds <dir> --target <target>`, then `POLAR_SCAFFOLD_DIR=<dir> nu infra/render.nu
<target>`; the manifests are in `manifests/`.

### 5. Your certificate in the browser

Neo4j's browser, and the other services for people, accept you by your Polar certificate. When `up.nu` reaches the
browser step it pauses. On **your machine**, close Librewolf, then:

```bash
bin/polar-dev cert <name>
```

and press Enter in the dev container. It copies your certificate out of the dev container and imports it, with the
cluster's CA, into Librewolf's certificate store, replacing any Polar certificate from before (it needs NSS's `certutil`
and `pk12util`: from Nix, or your package manager). `up.nu` then checks the Neo4j password and goes on. For another
browser, import `~/.config/polar-admin/<name>.crt`, its key and `ca.pem` yourself: **On a Mac, with minikube**, step 5, has
the commands for the macOS Keychain (Safari, Chrome, Edge) and for Firefox.

*On your own machine* (the variations of step 3), `up.nu` does this itself when it pauses at "Close Librewolf"
(`just user-cert-install <name>` does the same by hand).

### 6. Check that it works

`up.nu` ends with `=== Target local-minimal is up ===` and the pods it runs. For a browser on your machine:

```bash
bin/polar-dev forward              # Neo4j at https://localhost:7473
```

In the dev container, `just cluster-status` shows the nodes and every pod, and `kubectl -n polar get pods` Polar's own.
**Accessing the deployment**, below, has every service.

Things that look wrong and are not:

- **Restarts counting up on running pods.** The agents end on purpose, about once an hour, when their certificates are
  renewed, and their pod restarts in place (the same pod, its init containers again): a `RESTARTS` that grows is that,
  not a crash. After such a restart every container's last state reads exit 137, `RestartingAllContainers`: that is the
  kubelet's own record, not the agent's exit. On a bring-up, a scheduler observer can also restart once if Polar's git
  server is still starting.
- **`jira-observer` restarting** when no Jira is set up for the target: its site is `workloads.jira-observer.env` in the
  target's `config.nuon`, and its credential the signed `credentials` section that the seed seals from the target's sops
  files on a fresh cluster (see **Secrets setup**). Its pod restarts in place each time; nothing piles up.
- **`ErrImagePull: pull QPS exceeded` or `ImagePullBackOff`, briefly, when many pods start at once.** The kubelet's limit
  on registry requests (step 1). It retries by itself and the pods start.
- **A port-forward that stops answering** after a pod was replaced: forwards follow one pod. Run `just port-forwards-down`
  and then `just port-forwards-up` (in the dev container), or `bin/polar-dev forward` again (on your machine).
- **`the git server does not answer yet ... trying again` while seeding the schedules repository.** The git server had
  just restarted (it does when its settings change). The step retries and goes on.
- **`[polar] TLS certs missing -> building .#tlsCerts ... ERROR`** from a dev shell entered before the dev shell's TLS
  hook was retired (there is no dev PKI of its own any more). Leave the dev shell and enter it again, or `direnv reload`.

### 7. Day to day

```bash
bin/polar-dev attach               # log in to the dev container again
git -C /workspace pull             # (in the dev container) new code; then up.nu --dev to rebuild and roll it out
bin/polar-dev sops                 # copy your personal user.sops.yaml into the dev container's checkout (`up` does this too)
bin/polar-dev sync                 # copy ~/.config/polar-deploy and polar-admin back to your machine
bin/polar-dev down                 # sync, then stop the dev container; its volumes stay. --purge removes them too
```

- **The dev container keeps** `/workspace`, its Nix store (so builds are not redone), your shell history and
  `~/.config/polar-*` across restarts; `bin/polar-dev up` again after `down` brings the same one back.
- **To start over on a fresh cluster:** `bin/polar-dev sync` first (it keeps the deployment PKI and its signed history on
  your machine), rebuild the cluster (step 1, with the discovery-cache line), and `bin/polar-dev up` again: a new cluster is
  a new dev container, with your state copied in again, and step 4 from the top. `up.nu` notices when
  `~/.config/polar-admin` holds the previous cluster's CA and moves those files aside (it tells you where).
- **To switch targets** (`local-minimal` and `sandbox-local` share namespaces), start over on a fresh cluster with the
  other one.

**What to do next:** tell Polar what to observe. Repositories, credentials and the rules for accepting repositories other
agents propose are signed documents pushed to the cluster's git server with `polar-sign`; the walk-through, with every
command, is [docs/operations/git-private-repository.md](docs/operations/git-private-repository.md), and the design is in
[docs/architecture/signed-configuration.md](docs/architecture/signed-configuration.md). `infra/INFRA.md` explains the
catalog, targets and images behind `up.nu`.

### On a Mac, with minikube

The steps above, for an Apple-silicon Mac running a minikube cluster in Docker Desktop, with the images that are published
(nothing is built on your Mac). Each step points to the one above that says more.

**What it needs.** Measured on a single-node cluster running `local-minimal` (2026-10-28): Polar itself used about 7 GB of
memory and little CPU at rest, and about 9 GB of disk (6.6 GB of images, 1.9 GB of volumes). The dev container (step 3)
asks the cluster for 2 CPUs and 8 GiB of memory, which the cluster must be able to give it, and adds about 2.7 GB of image
and a 20 GiB volume. So, for the cluster:

| | Minimum | Recommended |
|---|---|---|
| Memory | 16 GiB | 20–24 GiB |
| CPUs | 4 | 6–8 (a bring-up is much busier than Polar at rest; this is an estimate) |
| Disk | 40 GB | 60 GB or more |

That memory is given to Docker Desktop's virtual machine, out of the Mac's own, so a Mac with 32 GB or more is the one to
use. On your Mac: Docker Desktop, minikube **v1.39.0 or later** (`brew install minikube`), `kubectl`, `git`, `ssh` with
your key in its agent (`ssh-add`), and a browser (Safari, Chrome or Edge, which use the macOS Keychain; Firefox works too).

**Docker Desktop.** In Settings, Resources, Advanced, give its virtual machine the CPUs, memory and disk above (a little more
memory than you will give minikube), then Apply and restart. Leave Docker Desktop's own Kubernetes off: minikube is the
cluster. (General Docker Desktop guidance, not tried on a Mac for this guide.)

**The steps, in order:**

1. **The cluster** (step 1). `--memory` must be under what Docker Desktop has:

   ```bash
   minikube start --driver=docker --cpus 6 --memory 20g --disk-size 60g --extra-config=kubelet.registry-pull-qps=0
   kubectl version              # Server Version v1.36 or later (minikube v1.39.0 starts v1.37 by default)
   kubectl get storageclass     # standard (default)
   ```

2. **Access to the secrets** (step 2): send your SSH public key to someone who can add it, pull, and check with `sops`.

3. **Your dev container in the cluster** (step 3), from your clone of the repository:

   ```bash
   export POLAR_DEV_SOPS_KEY=~/.ssh/<your key>
   bin/polar-dev up             # pulls the dev container for arm64; several minutes the first time
   ```

4. **Polar, from the published images** (step 4), in the dev container, from `/workspace`. Not `--dev`: that builds every
   image from source, which takes hours on a Mac.

   ```bash
   nu infra/up.nu local-minimal --import-from docker.io/daveman1010220 --user <name> --email <email>
   ```

   It copies the `arm64` images into the cluster, then deploys. Run the same command again after a failure or a pause.

5. **Your certificate in the browser** (step 5), by hand: `bin/polar-dev cert` installs it into Librewolf alone. When `up.nu`
   pauses at the browser step, on your Mac:

   ```bash
   bin/polar-dev sync           # copies ~/.config/polar-admin out of the dev container: <name>.crt, <name>.key, ca.pem
   cd ~/.config/polar-admin
   # Your certificate and key as one file the Keychain takes; it asks for a password to protect it, which the import asks for.
   # (macOS's own openssl. With Homebrew's OpenSSL 3, add -legacy, or the Keychain cannot read it.)
   openssl pkcs12 -export -in <name>.crt -inkey <name>.key -certfile ca.pem -name "Polar <name>" -out <name>.p12
   security import <name>.p12 -k ~/Library/Keychains/login.keychain-db
   # The cluster's CA, trusted, so the browser believes Polar's servers (it asks for your login password).
   security add-trusted-cert -r trustRoot -k ~/Library/Keychains/login.keychain-db ca.pem
   ```

   Then quit and reopen the browser, and press Enter in the dev container. Safari, Chrome and Edge read the Keychain; when
   Neo4j's page asks which certificate to use, choose "Polar `<name>`". **Firefox** keeps its own store instead: in its
   Settings, Privacy & Security, Certificates, View Certificates, import `<name>.p12` under Your Certificates and `ca.pem`
   under Authorities, trusting it to identify websites. A rebuilt cluster has a new CA: remove the old "Polar Internal CA"
   from Keychain Access (or Firefox) and do this again. (Standard macOS commands, not run on a Mac for this guide.)

6. **Open it** (step 6):

   ```bash
   bin/polar-dev forward        # then https://localhost:7473 (Neo4j)
   ```

   Step 6's checks are the same. Restart counts that grow, and `jira-observer` restarting when no Jira is set up, are
   expected (**Things that look wrong and are not**).

### Working environments

#### Dev container in the cluster (`bin/polar-dev`)

The same dev container as on a machine, as a pod in your cluster, so that your own machine needs only `kubectl`, `ssh`,
`git` and a browser, for a local cluster or one in the cloud alike: the builds, the tools and `up.nu` all run in the
cluster. You log in over SSH with your agent forwarded, so `git` in it uses your own key and no git key is kept in the
cluster.

`up` makes, in a namespace of your own (`polar-dev-<you>`):

- a ServiceAccount bound to `cluster-admin` (`up.nu` applies everything, CRDs and namespaces included; anyone who can exec
  into that namespace, or read its Secrets, can do what it can), and a kubeconfig for it, so `kubectl` and `up.nu` work
  inside as on a machine;
- volumes for the checkout (`/workspace`), the Nix store (seeded from the image, again whenever the image changes) and your
  state under your home: atuin's history, `~/.config/polar-admin` and `polar-deploy`, `~/.cache`, and `~/.ssh` with the
  pod's SSH host keys;
- your public key (the first in your agent, or `POLAR_DEV_SSH_KEY`) to log in with (the agent must hold a key either
  way: git in the pod uses it, forwarded), and the key `sops` decrypts with
  (`POLAR_DEV_SOPS_KEY`), as Secrets in your namespace. That one is a *private* key, kept wherever the cluster keeps its
  Secrets (a cloud provider's, for a cluster in the cloud): use a key for SOPS alone, not your main one;
- the pod, from `docker.io/daveman1010220/polar-dev:latest-<arch>` (amd64 and arm64; `POLAR_DEV_IMAGE` for another),
  headless, with SSH on 2223, on a node of that architecture. The architecture is the cluster's nodes', not your
  machine's; where they are of both, `POLAR_DEV_ARCH` says which. When `up` would change the image the pod runs (which
  seeds its Nix store again), it says so, and at a terminal asks first unless `POLAR_DEV_IMAGE` asked for it.
  Its SSH (dropbear) offers ECDSA P-256 as well as ed25519 and RSA host keys, and AES-GCM as well as chacha20 and
  AES-CTR (nix-container-lib `fff6abc`, `0771d7c`): a managed Mac's FIPS policy (`/etc/ssh/ssh_config.d/fips_ssh_config`,
  2026-10-30) takes neither ed25519 nor rsa-sha2-256 host keys and only AES-GCM ciphers. An image from before then
  refuses such a Mac; `build-image` (below) makes one until the published images are rebuilt.

It copies your `~/.config/polar-deploy` and `polar-admin` in, unless the pod has its own already, forwards
`127.0.0.1:2222` (`POLAR_DEV_PORT`) to the pod's SSH, clones `POLAR_DEV_REPO` (`POLAR_DEV_BRANCH`) into `/workspace` if it is
empty, and logs you in. The keys ssh trusts for the pod are kept per namespace
(`~/.ssh/polar-dev_known_hosts.<id>`), so a new cluster's pod is not mistaken for a changed host. Every setting is at the
top of `bin/polar-dev`; the pod's resources (2 CPUs and 8Gi of memory requested, with no limits, so it can use the whole node) and volumes (20Gi for
`/workspace`, 80Gi for the Nix store, 2Gi for your state) are in `infra/dev-pod/`. Nothing on your machine is mounted: the
volumes are the cluster's, from its default storage class (`POLAR_DEV_STORAGE_CLASS` for another), so on a cluster in the
cloud they are its disks. The image carries the Dhall the charts import (`src/flake/dhall-cache.nix`), so a render in the
pod fetches nothing; the dev shell on your machine has the same cache (`POLAR_DHALL_CACHE`).

```bash
bin/polar-dev attach        # log in again
bin/polar-dev cert [name]   # copy your certificate out and import it into Librewolf here
bin/polar-dev forward       # Neo4j (https://localhost:7473) for a browser here
bin/polar-dev sync          # copy ~/.config/polar-deploy and polar-admin back here
bin/polar-dev down          # sync, then stop the pod; --purge removes the namespace and its volumes too
bin/polar-dev build-image   # build the dev image in the pod and push it to the cluster's registry (below)
```

When the login fails, `up` and `attach` say why: whether the port-forward runs, ssh's refusal (`ssh -v`) and dropbear's
(the pod's log), and what to do. A local ssh policy (a managed Mac's FIPS one, in `/etc/ssh/ssh_config.d`) that accepts
none of the pod's host keys or ciphers is named with the files it comes from, and `up` warns before it starts when that
policy accepts none of the key types your agent holds. A clone the git server refuses (it knows none of your agent's keys,
which are listed) still logs you in, to an empty `/workspace`: add a key to your account there and `up` again clones it.

An image newer than the published one (a fix not yet on Docker Hub) is built in the pod itself, so your machine still
needs only `kubectl` and `git`: `bin/polar-dev build-image [tag] [target]` bundles this checkout's current commit (what is
committed, not the working tree), builds `devContainer` for the pod's architecture with the pod's Nix (as root, with
root's own cache), starts the target's registry in the cluster (`nu infra/cluster-registry.nu up`; the target is
`POLAR_TARGET`, else `local-minimal`), pushes `polar-dev:<tag>` (the short commit by default) to it, and prints the
`POLAR_DEV_IMAGE=localhost:30500/polar-dev:<tag> bin/polar-dev up` that puts the pod on it. The script's own tests are
`bash bin/tests/polar-dev.bash`.

#### Dev container on your machine

With `podman` (or `docker`) on your machine, the same dev container runs there:

```bash
just dev-container                 # builds it (or: podman pull docker.io/daveman1010220/polar-dev:latest-<arch>)

# state that has to outlive the container; create it first, so it is yours and not root's
mkdir -p ~/.config/polar-admin ~/.cache/polar-sign

podman run --rm -it --name polar-dev \
  --user 0 --userns=keep-id --network=host \
  -e CREATE_USER="$USER" \
  -e CREATE_UID="$(id -u)" \
  -e CREATE_GID="$(id -g)" \
  -v $PWD:/workspace \
  -v ${KUBECONFIG:-$HOME/.kube/config}:/kube/config:ro -e KUBECONFIG=/kube/config \
  -v $HOME/.config/polar-admin:/home/$USER/.config/polar-admin \
  -v $HOME/.cache/polar-sign:/home/$USER/.cache/polar-sign \
  -v $HOME/.ssh/<your key>:/run/sops-key:ro -e SOPS_AGE_SSH_PRIVATE_KEY_FILE=/run/sops-key \
  localhost/polar-dev:latest
```

With `docker`, run the same command as `docker run` and leave out `--userns=keep-id`, which is a podman option (docker,
running as root, keeps the host's user ids on bind mounts as they are).

What each mount is for:

- `--network=host`: the container reaches the cluster's API, a registry on `localhost:5000` and the port-forwards `up.nu`
  starts exactly as the host does. On a Mac, "host" is the Linux VM that runs the containers, not the Mac, so a browser on
  the Mac does not see the port-forwards; the dev container in the cluster avoids this.
- `/kube/config`: your kubeconfig, so `kubectl` and the tools reach your cluster. The cluster address in it must be
  reachable from the container (with `--network=host`, `127.0.0.1` is the host's).
- `~/.config/polar-admin`: your certificate, key and the cluster CA. They belong on the host, so they survive the container
  and your browser can use them.
- `~/.cache/polar-sign`: the tool's clone of the schedules repository.
- the SSH key, read-only: the private key you were added to the secrets file with, for `sops` (see **Secrets setup**). It
  is mounted outside your home on purpose: the container's entrypoint takes ownership of everything under `/home/$USER`,
  which a read-only mount refuses.

The container has the Polar tools (`polar-sign`, `polar-admin-grant`, `cassini-c2`, `cassini-client`, `user-cert-fetcher`,
`cert-client`, `cert-issuer-setup`) on its `PATH`, built as static binaries, together with `nu`, `just`, `bash`,
`kubectl`, `skopeo`, `dhall`, `sops`, `rage`, `git` and a Rust toolchain. It runs no containers itself, and needs none: with
the registry in the cluster, `up.nu` (`--dev` builds included) runs entirely in it. Importing the certificate into a
browser (step 5) is a host step.

Inside the container, at `/workspace`, `direnv allow` enters the dev shell; the first time it builds the full workspace
(every Rust agent), which takes a while. Keep the container (`podman stop`/`podman start`
rather than `--rm`) so later shells skip the rebuild.

**If your registry is on the host rather than in your cluster, and your cluster tooling lives on the host too**, get the
built images out of the container before they can be pushed: `just images-build` leaves Nix store symlinks in `result/`,
which only resolve inside the container. Copy them dereferenced (`cp -rL result/ /path/the/host/can/see/`), then push each
on the host (`skopeo copy docker-archive:<folder>/<image> docker://localhost:5000/<image>:<tag> --dest-tls-verify=false`;
`nu infra/images.nu list` prints every image name and tag). With the registry in the cluster none of this applies.

The step that drops a node's cached copy of a rebuilt image is written for one kind of dev node (a rootless `podman`
container running `containerd`, the default `POLAR_CLUSTER_RUNTIME=u7s`). On any other cluster (Docker Desktop's
Kubernetes, kind, k3d, Colima, minikube, a managed cluster) set `POLAR_CLUSTER_RUNTIME=none`. You do not need to clear the
node yourself: every image of Polar's that is named by a tag that moves (`latest`) is pulled with `imagePullPolicy:
Always`, so a pod that starts gets what your registry serves now. `up.nu` also warns, by name, when a running pod's image
is not the one the registry serves; if you see that on a cluster that has run Polar before, the node kept an old copy
(`minikube image rm <image>`, or recreate the cluster).

#### Tools on your machine

Without a dev container: [Nix](https://nixos.org) with flakes and `direnv` (the dev shell, `direnv allow` at the repository
root, provides the rest), or, with published images and nothing to build, `nushell`, `dhall`, `kubectl`, `skopeo`,
`sops`/`rage`, `git`, `gpgsm` (for a Flux target) and NSS's `certutil`/`pk12util` (for the browser) on your `PATH`, and
the Polar tools: `nu infra/tools.nu install ~/.local/bin` (**Installing the Polar tools**). Without them `up.nu` compiles
`polar-admin-grant` and `polar-sign` from this checkout, which needs a Rust toolchain. The tools are Linux binaries: on a
Mac, use a dev container. A container runtime is needed only for a registry on your machine (**Registries**). Outside the
dev shell, `sops` needs `SOPS_AGE_SSH_PRIVATE_KEY_FILE=~/.ssh/<your key>` (and `SOPS_AGE_BINARY=$(which rage)`).

### Registries

The cluster's nodes pull every Polar image from one registry, which the manifests name as the prefix of each image. There
are two ways to have it.

**In the cluster** (the shipped targets): nothing to run anywhere, and no container runtime. A target says so in its
`config.nuon`:

```nuon
cluster: { registry_in_cluster: { node_port: 30500 } }   # local_port (default 5000) and size (default 20Gi) are optional
```

`up.nu` applies it first (namespace `polar-registry`, a Deployment, a claim on the target's storage class, a NodePort
Service), before anything that pulls an image, and starts a port-forward this machine (or the dev container) pushes
through (`localhost:<local_port>`, set as `POLAR_REGISTRY` for the run). The nodes pull from `localhost:<node_port>` over
plain HTTP: a node's container runtime allows that for `localhost` with no configuration. The NodePort answers on a
node's localhost where kube-proxy serves NodePorts on 127.0.0.1, which its iptables mode does and its nftables mode does
not. A nix-usernetes node runs kube-proxy in nftables mode and has containerd send pulls for `localhost:30500` to its own
address, so keep `node_port: 30500` there; on another cluster in nftables mode, the nodes need the same for their port.
The registry image and, when the target deploys the storage chart, the local-path provisioner come from
Docker Hub, pinned by digest, since the registry cannot hold its own images before it exists. By hand: `nu
infra/cluster-registry.nu up <target>`. **It has no authentication yet:** anyone who can reach a node at the NodePort can
push and pull.

**One you bring**, for a target that names a registry instead (and leaves out `registry_in_cluster`). On your machine,
`just registry-host-up` starts one on `localhost:5000` (`just registry-host-status` says when it is up); that needs a container
runtime, which the scripts take as `podman` or `docker`, whichever is installed (`POLAR_CONTAINER_RUNTIME` to choose). For
any other registry there are two addresses, because the same registry has two names: the one this machine pushes to, and
the one your cluster's nodes pull from. Say both, in your environment (nothing in the repository to edit):

```bash
export POLAR_REGISTRY=localhost:5000                  # as this machine reaches it (default)
export POLAR_NODE_REGISTRY=polar-registry:5000        # as your cluster's nodes reach it
export POLAR_CLUSTER_RUNTIME=none                     # unless the cluster is a rootless podman dev node
```

`POLAR_NODE_REGISTRY` is the prefix of every image in the manifests (default `10.0.2.2:5000`, the host as a podman dev node
sees it; a target can also set `cluster.registry` in its `config.nuon`). With a registry you bring, you can import the
published images first (`nu infra/images.nu import --from docker.io/<account>`) and run `up.nu` with neither flag.

### Images

`infra/images.nuon` lists every image Polar can produce and where it
comes from: built from the flake with Nix, mirrored from a third-party
registry, or an archive built elsewhere (Neo4j). `up.nu --dev` builds
and pushes the ones the target needs. To work on them yourself, with the
target's registry (**Registries**). Each recipe prints the `infra/images.nu` command it runs. Those that push use the
registry in the target's cluster, through a forward they start, as `up.nu` does; the target is `POLAR_TARGET`, else
`local-minimal` (`just target=sandbox-local image cassini` for another), and `just target= …` pushes to `POLAR_REGISTRY`
instead:

```bash
nu infra/images.nu list            # every image, its tag and its source
just images-build <name>…          # build only (needs Nix; no registry, no cluster)
just images-push <name>…           # build and push to the target's registry
just images-mirror                 # copy the third-party images into it
just image <name>                  # every stage (alias: cluster-load)
just registry-up                   # the target's registry in its cluster, and the forward to it
just images-all                    # all of it, for every image
```

Give no names to `images-build`, `images-push` and `images-mirror` to
mean every image. A failing image does not stop the stage: the rest are
attempted and the stage ends with a summary.

To push to a different registry (a real cluster, or a customer
environment), set `POLAR_REGISTRY` to the address this machine pushes to
(default `localhost:5000`) and `POLAR_NODE_REGISTRY` to the address your
cluster's nodes pull from (**Registries**). Nothing in a target needs editing.

### Loading prebuilt images

If the images are published for you (a registry account such as `docker.io/<account>`), one command copies the ones for **this
machine's architecture** into the registry your cluster pulls from. It streams each image from one registry to the other with
`skopeo`: no Docker daemon, no `docker pull`, `docker save` or tarball in between.

For a target whose registry is in the cluster (the shipped ones), let `up.nu` do it, since the registry exists only once `up.nu`
has made it:

```bash
nu infra/up.nu local-minimal --import-from docker.io/<account> --user <name> --email <email>
```

For a registry of your own, or by hand (`nu infra/cluster-registry.nu up <target>` makes the one in the cluster and its forward
first, and prints the `POLAR_REGISTRY` to use):

```bash
just registry-host-up                                  # a registry on this machine, if you have not started it yet
nu infra/images.nu import --from docker.io/<account>   # amd64 or arm64, from `uname -m`; --arch to choose
```

It reads `<from>/polar-<image>:<tag>-<arch>` (for example `polar-cassini:latest-arm64`), writes each as `<image>:<tag>` in your
registry (`POLAR_REGISTRY`, default `localhost:5000`), copies the third-party images from their own registries (`--skip-mirrors` to
leave those), and records the digests. Name images to import only those (`nu infra/images.nu import cassini --from …`). Add
`--src-authfile <file>` for a private source and `--src-insecure` for a source that serves plain HTTP.

Neo4j is imported with the rest (it is published as `polar-neo4j`, from its own repository), and so is `polar-tools`, the image
the tools in **Installing the Polar tools** come out of. Then run
`nu infra/up.nu <target> --user <name> --email <email>` **without `--dev`**, so it uses the images already in the registry instead
of building them.

Doing it by hand, one image at a time:

```bash
skopeo copy --dest-tls-verify=false \
  docker://<registry>/<account>/polar-<name>:<tag>-<arch> docker://localhost:5000/<name>:<tag>
```

`nu infra/images.nu list` prints every image name and tag. Drop `--dest-tls-verify=false` for a registry that serves HTTPS, and add
`--dest-creds user:password` (or `--dest-authfile`) if it needs a login.

To publish images for others, `nu infra/images.nu publish --to docker.io/<account> --arch arm64 [--authfile <file>]` builds them for
that architecture and pushes them under those names. One that is not yours is built through the builder's emulation of it
(binfmt): it works, and it is slow (some hours for every image for `arm64` on an `x86_64` machine). It uses the login `podman login docker.io`
stored (or `--authfile`). A third-party mirror (for example `flux-source-controller`) is published only when you name it, and is copied from
its own registry for that architecture; `import` takes mirrors from their registries, so those copies are for people who cannot reach them.

### Installing the Polar tools

The tools you run yourself are `polar-sign` (sign and push configuration),
`polar-admin-grant` (create and claim accounts, bootstrap the CA),
`cassini-c2` (ask the broker to shut down), `cassini-client` (the broker's
client, for applications that are not written in Rust), `user-cert-fetcher`
(self-service user certificates), and `cert-client` and `cert-issuer-setup`
(the local PKI). They are built as **static binaries** for each
architecture, so a copy runs on any Linux of that architecture, and they
ship in the `polar-tools` image. Nothing has to be compiled to use them.
Copy them out into a directory of your choosing and put it on your `PATH`:

```bash
nu infra/tools.nu install ~/.local/bin
# or from an image somewhere else:
nu infra/tools.nu install ~/.local/bin --from docker://registry.example.org/polar-tools:latest
```

This needs only `skopeo` and `tar`. By default it reads
`polar-tools:latest` from `POLAR_REGISTRY` (default `localhost:5000`); load the
image into that registry as described under **Loading prebuilt images**,
or point `--from` at it. `polar-sign` also needs `git` on your `PATH`.

`up.nu`, the `Justfile`'s admin recipes and `cassini-shutdown.nu` use the
tool from your `PATH` when it is there. When it is not, they fall back to
building it from this checkout with `cargo run`, which is what a developer
with a Rust toolchain wants. To build the images and the tools yourself:
`just images-push polar-tools`, or `nix build .#polarPkgs.polarTools.tools`
for the binaries alone.

### Customize a target

A "target" is a named deployment profile under `infra/targets/`.
`local-minimal` is a small, single-node profile for local development;
`infra/targets/<name>/target.nu` lists what each target deploys, and
`infra/targets/<name>/overrides.dhall` is where you customize hostnames,
storage classes and other per-deployment values for the hand-written charts.
The registry is not one of them: a target keeps it in its cluster
(`cluster.registry_in_cluster` in its `config.nuon`) or names it
(`cluster.registry`, or `POLAR_NODE_REGISTRY`; **Registries**), and every chart reads
the one value that comes out of that.

### Loading data into Neo4j

If you have a Neo4j dump file (a `.dump` produced by `neo4j-admin
database dump`, for example from another environment's ingestion
history), load it into the running deployment with:

```bash
just neo4j-load-dump ~/path/to/your.neo4j.dump
```

It runs `infra/neo4j-load.nu`, which prints each `kubectl` call. It scales Neo4j down (on a Flux target, it suspends the
Kustomization that applies Neo4j first, so Flux does not scale it back up halfway), loads the dump into the `neo4j` database
in a temporary pod with Neo4j's own image and user, then scales Neo4j up again and resumes the Kustomization, even when the
load fails. The users and the password are left as they were: they are in the `system` database, which a dump of `neo4j`
does not touch. Neo4j is unavailable while it runs.

### Accessing the deployment

None of the in-cluster services are exposed outside the cluster by
default. With the dev container in the cluster, `bin/polar-dev forward` on your machine
forwards the one a browser needs (Neo4j); everything else is reached from inside
the dev container, where `up.nu` has already started what it uses. On your own machine
(or in a dev container there), a single recipe brings up detached, auto-restarting
port-forwards for everything you'd normally want reachable:

```bash
just port-forwards-up       # neo4j (7473/7687), keycloak (8080),
                            # jaeger (4318/16686), credentials-agent observer (8081)
just port-forwards-down     # stop them
```

These survive terminal closure and automatically retry on the
occasional dropped connection — a normal one-time step per session,
not something to leave running in a dedicated terminal.

- Neo4j Browser: `https://localhost:7473` (requires your user
  certificate; see below)
- Keycloak admin console: `http://localhost:8080`
- Jaeger traces: `http://localhost:16686`
- Credentials-agent observer (used by the admin tools): `http://localhost:8081`

### Admin access and your user certificate

People reach Neo4j Browser and the admin tools with a personal client
certificate, valid for one year and issued through the credentials-agent.
The certificate is tied to an account an admin provisions for you, and
your private key is generated on your own machine and never leaves it.
`up.nu` runs this flow for you (its "admin" and "browser" steps), so on a
fresh cluster you do not need to type it. Run it by hand to add another
person, or to redo a step. With the pods running and
`just port-forwards-up` active:

```bash
just admin-bootstrap                    # once per cluster
just admin-provision <user> <email>
just admin-claim <user> <email>
just user-cert-install <user>           # imports into Librewolf
```

- `admin-bootstrap` fetches the cluster CA into `~/.config/polar-admin/`
  and redeems the one-time bootstrap credential, writing
  `bootstrap-admin.crt` and `.key`. It works once per cluster.
- `admin-provision` signs, with the bootstrap certificate, the
  provisioning of the first (normal) admin account. The bootstrap
  certificate can provision exactly one account. Provisioning further
  accounts uses `polar-admin-grant provision-account` signed with an
  existing admin's claimed certificate (`--cert`/`--key`); a Justfile
  wrapper for that is planned.
- `admin-claim` runs as the account holder: it generates an ECDSA P-256
  keypair locally, claims the provisioned account, and writes
  `<user>.crt` and `<user>.key` next to the CA.
- `user-cert-install` bundles the certificate and imports it, together
  with the Polar CA, into Librewolf's certificate store (close the
  browser first). It is the only browser-specific step; for another
  browser, import the same files by hand.

Then browse to `https://localhost:7473` and log in to Neo4j with your
certificate. The `nix-neo4j` image this repository deploys sets the
`neo4j` user's password from the cluster's own secret on its first
start, so the agents can connect with nothing further for you to do.
(Only a Neo4j that still has its factory password needs the old
manual step: `up.nu` says so and shows the value to set. Avoid
repeated logins with a wrong password: Neo4j rate-limits failed
logins per user.)

A new certificate always involves an admin; certificates do not
self-renew. After a cluster rebuild the old CA is gone: `up.nu` moves the
previous cluster's files in `~/.config/polar-admin` aside for you. Keep a
backup of the bootstrap and user keys: losing the bootstrap material means
rebuilding the cluster.

### Bootstrapping Keycloak

`local-minimal`'s Keycloak deploys with no realm, client, or users
configured yet. A single idempotent recipe sets all of it up — safe to
re-run any time:

```bash
just keycloak-bootstrap
```

This creates the `polar` realm, a public OIDC client
(`browser-cert-fetcher`) used for human-facing certificate issuance,
and a test user, using credentials you provide via `sops` (see
`infra/targets/local-minimal/secrets.sops.yaml` — add
`keycloak_test_username`/`keycloak_test_password` there before
running).

### Using your own target

The sequence above assumes no particular Kubernetes distribution. To
deploy to an environment of your own (a shared cluster, a customer's),
make a target for it:

1. Do steps 1 to 3 of **Installation**: a cluster with storage, the secrets, and your dev container. The registry can be
   in your cluster, as the shipped targets keep it, or one you bring (**Registries**; a real cluster generally can't
   import a local image tarball directly the way `kind load` or `k3d image import` can).
2. Copy `infra/targets/local-minimal/` to a new target directory
   (the registry needs no edit: it stays in the cluster, or you set `POLAR_NODE_REGISTRY`; **Registries**), and
   adjust any hostnames/URLs specific to your environment. In particular, `cert-issuer`'s `issuers` list is where
   trust in Kubernetes and/or an external IdP (Keycloak, Okta, etc.)
   is configured — see the inline comments there for what each field
   means, especially `identityStrategy`'s `clusterDomain` (only needed
   if your cluster's kubelet runs with a non-default
   `--cluster-domain`).
3. Bring it up (step 4 of **Installation**): `nu infra/up.nu <your-target>`. It
   creates the namespaces and the CA, applies the target, and runs the admin
   flow, skipping whatever your target doesn't deploy.
4. Reach your cluster's credentials-agent observer however suits your
   setup and run the admin flow (see **Admin access and your user
   certificate**). The Justfile wrappers default to the dev port-forward
   at `http://localhost:8081`; set `POLAR_OBSERVER_URL` to point them
   elsewhere. The admin tool itself has no default and always takes an
   explicit `--observer-url`.

Some recipes in the `Justfile` (`cluster-reset`, `cluster-ctx` and a few
others) are conveniences for the maintainers' own development cluster and
are not needed to deploy anywhere else.

### Secrets setup

Two genuinely different kinds of secrets exist for a target like
`local`, each with its own file and its own SOPS config:

- **`secrets.sops.yaml`** — cluster-wide values shared by the whole
  team (the Keycloak test user's password, the Neo4j auth string,
  etc.). Encrypted to *every* team member's key, listed together in
  `.sops.yaml`. Committed to the repo, encrypted — never commit the
  plaintext.
- **`user.sops.yaml`** — your own personal credentials (your own
  GitLab token, your own Jira token). Encrypted to *only your own*
  key, via a separate, personal `.user.sops.yaml` config you create
  yourself. Gitignored — this one never gets committed at all, even
  encrypted. So a dev container's clone has none: `bin/polar-dev up` (or
  `bin/polar-dev sops`) copies yours in from your machine's checkout, still
  encrypted, unless the dev container has its own. It decrypts there with the
  key `POLAR_DEV_SOPS_KEY` names, which must be one yours is encrypted to.

Both use `sops`, both use your existing SSH keypair as your identity
(via `rage`'s SSH-key support) rather than a separate `age` key you'd
need to generate — but they're two separate files with two separate
configs, and mixing them up is the most common source of confusion
here.

#### 1. `my.env`

`cp example.env my.env`, then fill in every value marked `<required>`.
This file is sourced by `direnv` on shell entry and is gitignored —
never commit it.

#### 2. Get added as a recipient on the shared `secrets.sops.yaml`

This part you can't do for yourself the first time — someone who can
already decrypt the target's secrets has to add you.

Send them your SSH *public* key (`cat ~/.ssh/id_ed25519.pub`, or
whichever key you use) and ask them to:

1. Add it to `infra/targets/<target>/.sops.yaml`'s `age:` block. That file
   is local (gitignored), and the keys in it must be **comma-separated**
   (`key1,` newline `key2,` ...): sops reads a space-separated list as
   one recipient and silently encrypts to the first key only.
2. Run `sops --config infra/targets/<target>/.sops.yaml updatekeys infra/targets/<target>/secrets.sops.yaml`
   to re-encrypt the existing file with your key now included, then
   commit the file (the recipients live inside it, so you decrypt after
   you pull).

**If nobody can add you** (you are starting without access to anyone's key),
write a secrets file of your own for the target. The cluster cannot run
without two values, `neo4j_auth` (`neo4j/<password>`) and `graph_password`
(the same password); everything else is optional and the secrets step warns
about what it skips. From the repository root:

```sh
cd infra/targets/local-minimal
PUB=$(cut -d' ' -f1,2 ~/.ssh/id_ed25519.pub)       # your public key, not the private one
printf 'creation_rules:\n  - path_regex: secrets\\.sops\\.yaml$\n    age: >-\n      %s\n' "$PUB" > .sops.yaml
PW=$(openssl rand -hex 16)
printf 'neo4j_auth: "neo4j/%s"\ngraph_password: "%s"\n' "$PW" "$PW" > secrets.sops.yaml
SOPS_CONFIG=.sops.yaml sops --encrypt --in-place secrets.sops.yaml
```

That replaces the encrypted file that came with the checkout, which you could
not read. Do not commit it: `git update-index --skip-worktree
infra/targets/local-minimal/secrets.sops.yaml` keeps it out of `git status`.
(Tried with sops 3.13.)

If your SSH private key isn't at the default path the devShell
assumes, set `SOPS_AGE_SSH_PRIVATE_KEY_FILE` in your own `my.env` to
point at it (see `example.env`'s own comment on this).

Once you're a listed recipient, and you have pulled the commit that
re-encrypted the file with your key, you can decrypt and edit
`secrets.sops.yaml` directly:

```sh
# Decrypt for inspection:
SOPS_AGE_SSH_PRIVATE_KEY_FILE=~/.ssh/id_github sops --decrypt secrets.sops.yaml

# Edit in place (opens your $EDITOR on the decrypted content, re-encrypts on save):
SOPS_AGE_SSH_PRIVATE_KEY_FILE=~/.ssh/id_github sops secrets.sops.yaml
```

`SOPS_AGE_SSH_PRIVATE_KEY_FILE` is what tells `sops` which SSH private
key to decrypt with — point it at whichever SSH key you had added as a
recipient in step 2. Without it, `sops` tries `~/.ssh/id_ed25519` and
then `~/.ssh/id_rsa`. Do not use `SOPS_AGE_KEY_FILE` for this: it takes
native age keys (`AGE-SECRET-KEY-1...`) only, and given an SSH key it
reports `unknown identity type`.

If decryption fails with `no identity matched any of the recipients`,
look at the recipients `sops` lists. One entry holding several
space-separated keys means your copy was encrypted to the first key
only: pull, and if it persists, ask for step 2 again. If they are
listed one per entry, compare yours with `ssh-keygen -y -f <your key>`:
the private key you point at is not the one you sent.

#### 3. Set up your own `user.sops.yaml`

This one you genuinely can do for yourself — it's your own file,
encrypted only to your own key, and nobody else needs to grant you
access to it.

1. **Create your own, personal SOPS config.** `user.sops.yaml`'s own
   config lives in a separate file, `.user.sops.yaml`, scoped to just
   this one file (mirroring `.sops.yaml`'s own shape, but with a
   single recipient — you). There's no example file to copy for this
   step today; create `infra/targets/<target>/.user.sops.yaml`
   directly, with your own SSH public key as the sole recipient:

   ```yaml
   creation_rules:
     - path_regex: user\.sops\.yaml$
       age: >-
         ssh-ed25519 AAAA...your own public key here...
   ```

   This file is gitignored, same as `user.sops.yaml` itself — it's
   personal to your own machine.

2. **Create your own secrets file from the example.**

   ```sh
   cp user.sops.yaml.example user.sops.yaml
   ```

   Fill in your own values (your GitLab token, your Jira token, etc.).
   The Jira token (`jira_token`, with `jira_email` for a Jira Cloud API token) is not made a
   Secret: on a fresh cluster `up.nu`'s seed seals it once into jira-observer's signed
   `credentials` section. After that it is not read again; to change the token, run
   `polar-sign template jira-observer credentials --out creds.json`, put `-` for the secret
   and a higher version, and pipe the new token into
   `polar-sign sign creds.json --user <you> --push` (`docs/architecture/jira.md`, "The credential").

3. **Encrypt it**, pointing `sops` at your own config via
   `SOPS_CONFIG` — this is the one genuinely different mechanism
   between the two files, and the part most likely to trip you up if
   you're used to step 2's own commands (the key is still found the
   same way, through `SOPS_AGE_SSH_PRIVATE_KEY_FILE`):

   ```sh
   SOPS_CONFIG=.user.sops.yaml sops --encrypt --in-place user.sops.yaml
   ```

   `SOPS_CONFIG` tells `sops` which `creation_rules` to use (yours,
   not the shared `.sops.yaml`); `sops` still uses your same SSH key
   to actually do the encrypting, via the same `rage` SSH-key support
   as before.

4. **Edit it later** the same way:

   ```sh
   EDITOR=$(which nvim) SOPS_CONFIG=.user.sops.yaml sops user.sops.yaml
   ```

   Or decrypt for inspection only:

   ```sh
   SOPS_CONFIG=.user.sops.yaml sops --decrypt user.sops.yaml
   ```

#### 4. Changing a value

Edit the file (the commands above) and run `up.nu` again: the secrets
step updates each Secret whose value the files now give differently,
and names the deployments to restart so they read it (a pod reads a
Secret when it starts). A Secret that already holds what the files give
is left alone. A Secret the files have no value for keeps whatever the
cluster holds, so a value set by hand is not overwritten by the
placeholder. The broker's shutdown token is made in the cluster once
and never replaced. Neo4j takes its password from the Secret on its
first start only, so a later change to `neo4j_auth` does not change the
database's password.

Store each value as it is, not base64-encoded: Kubernetes encodes
Secret values itself, so an encoded token reaches the agent still
encoded, and is refused. The secrets step warns when a value looks
encoded, naming its key and file (never the value).

#### Quick reference: which mechanism for which file

| File | Config used | Env var |
|---|---|---|
| `secrets.sops.yaml` (shared) | `.sops.yaml` (gitignored, multi-recipient; only needed to encrypt, not to decrypt) | `SOPS_AGE_SSH_PRIVATE_KEY_FILE=<path to your key>` |
| `user.sops.yaml` (personal) | `.user.sops.yaml` (gitignored, your own single-recipient config, which you create) | `SOPS_CONFIG=.user.sops.yaml` |

### Iterating on an agent

There is no static, offline-generated dev PKI any more: every certificate
comes from `cert-issuer`, signed by the cluster's own CA. To iterate on an
agent, rebuild its image, push it and restart its Deployment; its new pod
pulls the image again, and `polar-init` fetches a new certificate on startup:

```bash
just image <image-name>      # build it and push it to the cluster's registry
kubectl -n polar rollout restart deployment/<workload>
```

(`nu infra/images.nu list` has the image names; `kubectl -n polar get deploy`
the workloads.)

Human access (Neo4j Browser and the admin tools) uses a user certificate
claimed through the admin flow; see **Admin access and your user
certificate**, above.

## Validating Keycloak certificate issuance

Kubernetes workloads (the ingestion agents and services) get their certificates
automatically, with no manual steps. People normally get theirs through
the admin flow (see **Admin access and your user certificate**).
Separately, `cert-issuer` can issue a certificate to a **human
authenticated against Keycloak** (or another configured identity
provider). There is no polished client tool for that route yet, so this
is a manual walkthrough of the real, verified sequence, useful for
confirming the deployment is actually working end to end.

1. Get a token from Keycloak (after `just port-forwards-up` and `just
   keycloak-bootstrap`, using the same test credentials from
   `secrets.sops.yaml`):

```bash
   TOKEN=$(curl -s -X POST http://localhost:8080/realms/polar/protocol/openid-connect/token \
     -H "Content-Type: application/x-www-form-urlencoded" \
     -d "grant_type=password" \
     -d "client_id=browser-cert-fetcher" \
     -d "username=<your test username>" \
     -d "password=<your test password>" \
     | jq -r '.access_token')
```

2. Decode it to see the exact identity/scope values this specific
   deployment produces (these vary per-realm; don't assume the
   values shown here):

```bash
   echo $TOKEN | cut -d. -f2 | base64 -d | jq .
```

   Look for `preferred_username` and `identity_scope` — the resulting
   certificate's identity is `<preferred_username>.<identity_scope>`.

3. Generate a keypair and CSR whose SAN matches that exact identity:

```bash
   openssl ecparam -genkey -name prime256v1 -noout -out key.pem
   openssl req -new -key key.pem -out csr.pem -subj "/" \
     -addext "subjectAltName=DNS:<preferred_username>.<identity_scope>"
```

4. Port-forward `cert-issuer` itself (not brought up by
   `port-forwards-up`, since workloads reach it in-cluster and this is
   only needed for this kind of manual, from-your-laptop check), and
   save the cluster CA so you can verify its certificate:

```bash
   kubectl port-forward -n polar svc/cert-issuer 8443:8443 &
   kubectl -n polar get configmap polar-ca -o jsonpath='{.data.ca\.pem}' > ca.pem
```

5. Request the certificate. `cert-issuer` only speaks TLS, and its
   certificate is issued for its in-cluster name, so verify against that
   name with `--resolve`:

```bash
   CSR_JSON=$(jq -n --arg csr "$(cat csr.pem)" \
     '{csr_pem: $csr, cert_type: "CLIENT", extra_sans: []}')

   curl -s --cacert ca.pem \
     --resolve cert-issuer.polar.svc.cluster.local:8443:127.0.0.1 \
     -X POST https://cert-issuer.polar.svc.cluster.local:8443/issue \
     -H "Authorization: Bearer $TOKEN" \
     -H "Content-Type: application/json" \
     -d "$CSR_JSON" \
   | jq .
```

   A successful response includes `certificate_pem` and `ca_chain_pem`.
   Decode the certificate (`openssl x509 -noout -text`) and confirm its
   Subject Alternative Name matches what you expected from step 3.

## License

Licensed under a MIT (SEI)-style license — see license.txt, or contact
permission@sei.cmu.edu for full terms.
