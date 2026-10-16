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

# ACE — Agile Capacity Estimator

A team completion-projection dashboard for Agile/Scrum teams. ACE reads
work-item data from a Neo4j graph, runs Monte Carlo simulation against
each team's actual historical performance, and shows you — honestly,
with real confidence bands, not a single optimistic line — when a
Program Increment is actually going to finish.

## What ACE does

- **Projection Chart** — pick a team and PI, and see a probability
  distribution for the completion date: probability of finishing by the
  committed date, projected completion date, and projected percent
  complete at PI end.
- **Realtime Alerts** — WIP-delay and completion-probability warnings
  surfaced as soon as a team's actual pace diverges from what's needed
  to hit the committed date.
- **Team Impact Adjustment** — compare multiple teams side by side to
  see where cross-team dependencies are putting a shared PI date at
  risk.

The projection engine is PERT-based Monte Carlo: it samples from each
team's own historical velocity (not an assumed one), and — when
per-issue data is available — models per-issue completion individually
with team-size contention, rather than a single aggregate burn-down
line. See [src/ace/ARCHITECTURE.md](src/ace/ARCHITECTURE.md) for the
full technical detail.

## Using ACE

Once deployed (see Installation below), open the dashboard, select a
team and PI increment, and click **Get Graph**. Advanced options let
you adjust the projection start date and training window. See
[src/ace/README.md](src/ace/README.md) for local development and
direct container usage.

## Installation

ACE ships as a container image, built reproducibly with Nix and
deployed to Kubernetes.

### Prerequisites

- A Kubernetes cluster (any distribution — this pipeline doesn't
  assume a specific one; see **Using your own target**, below)
- A Neo4j instance populated with work-item data (see **Data source**,
  below)
- [Nix](https://nixos.org) with flakes enabled, `podman`, `direnv`,
  [`just`](https://github.com/casey/just), `nushell` and `kubectl` — or
  use the provided dev container (see below), which already has the
  build tools. The full list is in **Bring a cluster up**, below.
  **If you are given prebuilt images** (see **Loading prebuilt images**,
  under **Images**) you do not build them and do not need Nix for that.
  You also do not compile anything to install: the command-line tools
  `up.nu` runs (`polar-admin-grant`, `polar-sign`, `cassini-c2`) come as
  static binaries in the `polar-tools` image (see **Installing the Polar
  tools**). What `up.nu` still needs on your `PATH`: `nushell`, `dhall`,
  `kubectl`, `skopeo`, `podman`, `sops`/`rage` and `git`. The dev
  container has all of them except `podman` (below), so use it unless you
  already have those installed

### Dev container

If you don't already have Nix/`just`/`skopeo` set up, `just
dev-container` builds a ready-to-use development environment with
everything the build pipeline needs:

```bash
just dev-container

podman run --rm -it --name polar-dev \
  --user 0 --userns=keep-id \
  -e CREATE_USER="$USER" \
  -e CREATE_UID="$(id -u)" \
  -e CREATE_GID="$(id -g)" \
  -v $PWD:/workspace \
  localhost/polar-dev:latest
```

**On an ARM Mac:** you need a dev container image to get started.
Reach out to the developers if you do not have one.

The container has the Polar tools (`polar-sign`, `polar-admin-grant`,
`cassini-c2`, `cassini-client`, `user-cert-fetcher`, `cert-client`,
`cert-issuer-setup`) on its `PATH`, built as static binaries, together with
`nu`, `just`, `kubectl`, `skopeo`, `dhall`, `sops`, `rage`, `git` and a Rust
toolchain. It does not run `podman` itself; see **Bring a cluster up** for
which steps need it.

Inside the container, at `/workspace`:

```bash
direnv allow
```

The first `direnv allow` builds the full workspace (every Rust agent,
plus ACE's Python environment) — this takes a while the first time.
Once built, keep the container running (`podman stop`/`podman start`
rather than `--rm`-ing it) so subsequent shells skip the rebuild
entirely.

**If your cluster tooling (`podman`, `skopeo`, and your Kubernetes
distribution itself) lives on the host rather than inside the dev
container** — common on a Mac, where the dev container only provides
the Linux build environment — you need to get the built images out of
the container before they can be pushed. `just images-build` leaves
Nix store symlinks in `result/`; they only resolve inside the
container's own filesystem. Copying them with a plain `cp` copies the
symlink itself, not the image data — `ls` will still show the file on
the host side, but any tool that opens it will report it as not found.
Dereference on copy instead:

```bash
cp -rL result/ /path/to/a/folder/the/host/can/see/
```

Then, on the host, push each archive to the registry yourself:

```bash
skopeo copy docker-archive:<folder>/<image> \
  docker://localhost:5000/<image>:<tag> --dest-tls-verify=false
```

(`nu infra/images.nu list` prints every image name and tag.)

The step that drops a node's cached copy of a rebuilt image is written
for one kind of dev node (a rootless `podman` container running
`containerd`, the default `POLAR_CLUSTER_RUNTIME=u7s`). On any other
cluster (Docker Desktop's Kubernetes, kind, k3d, Colima, a managed
cluster) set `POLAR_CLUSTER_RUNTIME=none`, e.g.
`POLAR_CLUSTER_RUNTIME=none just image <name>`.

### Bring a cluster up: the whole sequence

This is the path from a Kubernetes cluster to a running Polar cluster.
Polar does not care which Kubernetes distribution you use: anything that
meets the list in step 1 will do, and setting one up is outside this
document. Each step says what it does and what to expect; the sections
after it go into detail. Every command is run from the repository root
unless it says otherwise.

**What you need before you start** (the dev shell from `direnv allow`
provides most of the tools):

- Nix with flakes, `podman` (the local registry in step 2 runs in it),
  `just`, `nushell`, `kubectl`, `skopeo`, `dhall`, `sops` and `rage`.
- A Kubernetes cluster, and admin access to it (step 1).
- A checkout of the sibling repository **`nix-neo4j`** at
  `~/Documents/projects/nix-neo4j`, which builds the Neo4j image Polar
  deploys.
- Your SSH key listed as an age recipient of the target's secrets file
  (see **Secrets setup**), so `sops` can decrypt it.

**1. Have a cluster, and point `kubectl` at it.**

```bash
export KUBECONFIG=<path to your kubeconfig>
kubectl get nodes      # at least one node, Ready
```

What Polar asks of the cluster:

- **A registry its nodes can pull from** (step 2), and the address the
  *nodes* use for it in `registry` in `infra/config/cluster.nuon`. That
  address is often not the one your machine pushes to.
- **A storage class** for persistent volumes. The `local-minimal` target
  installs a local-path provisioner and a class named `managed-csi`
  (`storage.class` in the same file). On a cluster that has its own class,
  set `storage.class` to it.
- **The cluster's DNS domain** in `domain` in the same file, if it is not
  `cluster.local`.
- **The `pkcs8_key_parser` kernel module** loaded on the nodes
  (`sudo modprobe pkcs8_key_parser`): the agents' key custody needs it.
- **Pods that can resolve names and reach the internet**, if you want to
  observe repositories on GitHub or GitLab. `up.nu` checks this at the end
  and says what it found. A DNS search domain on the node that makes every
  lookup slow (some university and corporate networks) is a common cause of
  slow or timed-out pods; remove it from the node's resolver.

**2. Have a registry** the cluster pulls images from, and say where it is.
For a local cluster, `just registry-up` starts one on `localhost:5000`:

```bash
just registry-up        # a registry container on localhost:5000
just registry-status    # prints "registry reachable" when it is up
```

For any other registry, set `POLAR_REGISTRY` to the address this machine
pushes to (default `localhost:5000`) and `registry` in
`infra/config/cluster.nuon` to the address the cluster's nodes pull from.
Also set `POLAR_CLUSTER_RUNTIME=none` unless your cluster is a rootless
podman dev node (see **Images**).

**3. Build the Neo4j image.** In the `nix-neo4j` checkout:

```bash
nix build -L .#neo4j-image -o result-neo4j-image
```

`infra/images.nuon` expects the archive at
`~/Documents/projects/nix-neo4j/result-neo4j-image`; set
`POLAR_NEO4J_IMAGE` to its path if you keep it elsewhere. This image sets
the `neo4j` user's password from `NEO4J_AUTH` on its first start, so there
is no password to change by hand later.

**4. Make sure `sops` can find your key.** Inside the dev shell
`SOPS_AGE_BINARY` is already set; set `SOPS_AGE_SSH_PRIVATE_KEY_FILE` to
the private key you were added with (put it in your `my.env`, see
`example.env`). Outside the dev shell, export both:

```bash
export SOPS_AGE_BINARY=$(which rage)
export SOPS_AGE_SSH_PRIVATE_KEY_FILE=$HOME/.ssh/<your key>
```

**5. Bring Polar up.**

First put the tools on your `PATH` (**Installing the Polar tools**,
below): `nu infra/tools.nu install ~/.local/bin`. Without them `up.nu`
falls back to compiling `polar-admin-grant` and `polar-sign` from this
checkout, which needs a Rust toolchain and takes a few minutes.

```bash
nu infra/up.nu local-minimal --dev --user <name> --email <email>
```

It prints its plan first, then builds and pushes the images the target
needs (`--dev`; the first cold build takes a long while), creates the
namespaces and the cluster CA, applies the target, waits for the core
pods, starts the port-forwards, runs the admin flow and signs and pushes
the cluster's first policy. It stops for a person only where one is
needed. After any failure or pause, **run the same command again**: every
step checks whether it is already done.

**6. If it stops at the policy step, set the git server's administrator.**
The Polar git server takes its administrators by identity, and an identity
belongs to one cluster, so a new cluster needs its own. The step says:
"the Polar git server does not know `<id>` as an administrator". Read the
first DNS name in your new certificate:

```bash
openssl x509 -in ~/.config/polar-admin/<name>.crt -noout -ext subjectAltName
```

put it as the value of `POLAR_GIT_SERVER_ADMINS` for `polar-git-server` in
`infra/targets/local-minimal/config.nuon`, and run the `up.nu` command
again. (You do this once per fresh cluster, since a reset gives new ids.)

**7. Import your certificate into the browser.** When `up.nu` pauses at
"Close Librewolf", close it and press on: it imports the cluster's CA
and your user certificate (`just user-cert-install <name>` does the same
by hand). For another browser, import the same files from
`~/.config/polar-admin/` yourself. Nothing else is needed for Neo4j.

**8. Check that it works.**

```bash
just cluster-status                 # nodes and every pod
kubectl -n polar get pods
just port-forwards-up               # then browse https://localhost:7473
```

Things that look wrong and are not:

- **Pods in `Error` next to a running replacement.** The agents are
  Kubernetes Jobs and exit on purpose, about once an hour, when their
  certificates are renewed; the Job starts a fresh pod. Look at the
  newest pod.
- **`jira-observer` failing** when no Jira is set up for the target: its
  credentials come from the target's secrets (see **Secrets setup**).
- **A port-forward that stops answering** after a pod was replaced:
  forwards follow one pod. Run `just port-forwards-down` and then
  `just port-forwards-up`.
- **A Job that is slow to replace a pod you deleted by hand.** The Job
  controller waits longer after each failure, up to about six minutes.

**To start over:** wipe the cluster the way your distribution does,
then repeat steps 1, 5, 6 and 7. `up.nu` notices when
`~/.config/polar-admin` holds the previous cluster's CA and moves those
files aside (it tells you where).

**What to do next:** tell Polar what to observe. Repositories,
credentials and the rules for accepting repositories other agents propose
are signed documents pushed to the cluster's git server with
`polar-sign`; the walk-through, with every command, is
[docs/operations/git-private-repository.md](docs/operations/git-private-repository.md),
and the design is in
[docs/architecture/signed-configuration.md](docs/architecture/signed-configuration.md).
`infra/INFRA.md` explains the catalog, targets and images behind
`up.nu`.

### Images

`infra/images.nuon` lists every image Polar can produce and where it
comes from: built from the flake with Nix, mirrored from a third-party
registry, or an archive built elsewhere (Neo4j). `up.nu --dev` builds
and pushes the ones the target needs. To work on them yourself, with the
registry from step 2 running:

```bash
nu infra/images.nu list            # every image, its tag and its source
just images-build <name>…          # build only (needs Nix; no registry, no cluster)
just images-push <name>…           # build and push to the registry
just images-mirror                 # copy the third-party images into the registry
just image <name>                  # every stage, then drop the node's cached copy
just images-all                    # all of it, for every image
```

Give no names to `images-build`, `images-push` and `images-mirror` to
mean every image. A failing image does not stop the stage: the rest are
attempted and the stage ends with a summary.

To push to a different registry (a real cluster, or a customer
environment), set `POLAR_REGISTRY` (default `localhost:5000`), and
see **Using your own target**, below, for pointing a target's
`overrides.dhall` at it.

### Loading prebuilt images

If someone gives you a registry that already holds the images (instead of
you building them), copy them into the registry your cluster pulls from
with `skopeo`. It streams each image from one registry to the other: no
Docker daemon, no `docker pull`, `docker save` or tarball in between.

```bash
SRC=<registry and path the images are published under>
DEST=localhost:5000                      # the registry your cluster pulls from
nu infra/images.nu list                  # the names and tags to copy
skopeo copy --dest-tls-verify=false \
  docker://$SRC/<name>:<tag> docker://$DEST/<name>:<tag>
```

Copy every image in that list except Neo4j (build it yourself, step 3
above, then `just images-push neo4j`); that includes `polar-tools`, the image
the tools in **Installing the Polar tools** come out of. Keep
the names and tags the list gives. Drop
`--dest-tls-verify=false` for a registry that serves HTTPS, and add
`--dest-creds user:password` (or `--dest-authfile`) if it needs a login.
The images are built for one architecture; copy the ones that match your
nodes. Then run `nu infra/up.nu <target> --user <name> --email <email>`
**without `--dev`**, so it uses the images already in the registry instead of
building them.

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
`infra/targets/<name>/overrides.dhall` is where you customize
image references, hostnames, and other per-deployment values.

### Deploy

Render the target and bring the cluster up with the orchestrator, from the
repo root with `KUBECONFIG` pointing at the cluster:

```bash
nu infra/up.nu local-minimal --dev --user <name> --email <email>
```

`infra/render.nu` only renders (it never touches a cluster); `infra/up.nu`
renders and then applies. It is driven by the target's own config, so it runs
only the steps that target needs and prints the plan first, with the reason for
every skipped step. For `local-minimal` that is: images (`--dev`), namespaces,
the cluster CA, secrets and manifests in the target's apply order, a wait for
the core pods, the port-forwards, the admin flow and the browser certificate.
It stops only where a person is needed (closing the browser, the Neo4j
password), and every step checks whether it is already done, so after a
failure you re-run the same command to resume.

The CA step runs `polar-admin-grant bootstrap-ca`, which needs a Kubernetes
admin credential. It creates the cluster's own CA once (the `cert-issuer-ca`
Secret and the `polar-ca` ConfigMap) and refuses to run again if they already
exist, so replacing a CA means rebuilding the cluster. Everything else in the
deployment gets its certificates from that CA.

Swap `local-minimal` for whichever target you've set up. To only render, run
`nu infra/render.nu <target>`; it writes every chart to `manifests/`.

For `local-minimal` specifically, this deploys the full stack: Neo4j,
Cassini, `cert-issuer`, the credentials-agent and its observer, ACE,
the ingestion agents, and Keycloak with its own Postgres backing store
(an optional identity-provider route for human-facing certificate
issuance).

ACE runs as a Kubernetes `Job` (not a `Deployment`): it requests a
long-lived mTLS client certificate from the cluster's certificate
infrastructure via an init container on every pod creation, which is
what lets it recover automatically if its certificate or environment
ever needs to be refreshed. It authenticates to Neo4j over mutual TLS
and holds no other cluster permissions.

Wait for the pods to come up (`kubectl -n polar get pods -w`) before
continuing with **Accessing the deployment** and **Admin access and
your user certificate**, below.

### Loading data into Neo4j

If you have a Neo4j dump file (a `.dump` produced by `neo4j-admin
database dump`, for example from another environment's ingestion
history), load it into the running deployment with:

```bash
just neo4j-load-dump ~/path/to/your.neo4j.dump
```

This scales Neo4j down, restores the dump via a temporary loader pod,
fixes file ownership, scales Neo4j back up, and resets the `neo4j`
user's password to match the cluster's own secret — the whole sequence
is scripted and safe to run against a live deployment, but Neo4j will
be briefly unavailable while it does.

### Accessing the deployment

None of the in-cluster services are exposed outside the cluster by
default. A single recipe brings up detached, auto-restarting
port-forwards for everything you'd normally want reachable from your
own machine:

```bash
just port-forwards-up       # neo4j (7473/7687), keycloak (8080), ace (8050),
                            # jaeger (4318/16686), credentials-agent observer (8081)
just port-forwards-down     # stop them
```

These survive terminal closure and automatically retry on the
occasional dropped connection — a normal one-time step per session,
not something to leave running in a dedicated terminal.

- ACE's dashboard: `http://localhost:8050`
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

1. Do steps 1 and 2 of the sequence: a kubeconfig, and a registry the
   nodes can pull from (a real cluster generally can't import a local
   image tarball directly the way `kind load` or `k3d image import` can).
2. Copy `infra/targets/local-minimal/` to a new target directory,
   update `overrides.dhall`'s image references to point at your
   registry, and adjust any hostnames/URLs specific to your
   environment. In particular, `cert-issuer`'s `issuers` list is where
   trust in Kubernetes and/or an external IdP (Keycloak, Okta, etc.)
   is configured — see the inline comments there for what each field
   means, especially `identityStrategy`'s `clusterDomain` (only needed
   if your cluster's kubelet runs with a non-default
   `--cluster-domain`).
3. Bring it up (see **Deploy**, above): `nu infra/up.nu <your-target>`. It
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
  encrypted.

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

1. Add it to `infra/targets/<target>/.sops.yaml`'s `age:` block.
2. Run `sops updatekeys infra/targets/<target>/secrets.sops.yaml` to
   re-encrypt the existing file with your key now included.

If your SSH private key isn't at the default path the devShell
assumes, set `SOPS_AGE_SSH_PRIVATE_KEY_FILE` in your own `my.env` to
point at it (see `example.env`'s own comment on this).

Once you're a listed recipient, you can decrypt and edit
`secrets.sops.yaml` directly:

```sh
# Decrypt for inspection:
SOPS_AGE_KEY_FILE=~/.ssh/id_github sops --decrypt secrets.sops.yaml

# Edit in place (opens your $EDITOR on the decrypted content, re-encrypts on save):
SOPS_AGE_KEY_FILE=~/.ssh/id_github sops secrets.sops.yaml
```

`SOPS_AGE_KEY_FILE` here is what tells `sops`/`rage` which private key
to use for decryption — point it at whichever SSH key you had added
as a recipient in step 2.

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

3. **Encrypt it**, pointing `sops` at your own config via
   `SOPS_CONFIG` rather than `SOPS_AGE_KEY_FILE` — this is the one
   genuinely different mechanism between the two files, and the part
   most likely to trip you up if you're used to step 2's own
   commands:

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

#### Quick reference: which mechanism for which file

| File | Config used | Env var |
|---|---|---|
| `secrets.sops.yaml` (shared) | `.sops.yaml` (repo-tracked, multi-recipient) | `SOPS_AGE_KEY_FILE=<path to your key>` |
| `user.sops.yaml` (personal) | `.user.sops.yaml` (gitignored, your own single-recipient config, which you create) | `SOPS_CONFIG=.user.sops.yaml` |

### Just the container

To build and run ACE's container directly, without the full Kubernetes
deployment:

```bash
cd src/ace
nix build .#aceImage
podman load -i result-ace
podman run -p 8080:8080 \
  -e GRAPH_ENDPOINT=bolt://<host>:7687 \
  -e GRAPH_USER=neo4j \
  -e GRAPH_PASSWORD=<password> \
  ace-viz:latest
```

### Iterating on an agent

There is no static, offline-generated dev PKI any more: every certificate
comes from `cert-issuer`, signed by the cluster's own CA. To iterate on an
agent, rebuild its image, push it and bounce its pod; Jobs create a
fresh pod, and `polar-init` fetches a new certificate on startup:

```bash
just image <image-name>      # build, push, drop the node's cached copy
kubectl -n polar delete pod -l job-name=<job-name>
```

(`nu infra/images.nu list` has the image names. Delete a pod once: the
Job controller waits longer after each pod that fails, so deleting several
in a row delays the replacement.)

Human access (Neo4j Browser and the admin tools) uses a user certificate
claimed through the admin flow; see **Admin access and your user
certificate**, above.

## Validating Keycloak certificate issuance

Kubernetes workloads (ACE, the ingestion agents) get their certificates
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

## Data source

ACE reads from Neo4j — it does not talk to Jira, GitLab, or any other
source system directly. The graph it reads from is populated by
**Polar**, the platform this repository is built on: a set of
Cassini-connected ingestion agents (Jira, GitLab, Kubernetes, and
others) that observe source systems and write structured work-item
data into Neo4j. Polar also provides the certificate infrastructure
(cert-issuer, Keycloak) ACE and its users authenticate through, and the
Nix/Dhall/Kubernetes build and deployment pipeline this README's
install steps use.

If you're working on ACE itself, you'll mostly live in src/ace/. If
you're working on the ingestion agents, certificate infrastructure, or
deployment pipeline, see src/agents/ and infra/.

## License

Licensed under a MIT (SEI)-style license — see license.txt, or contact
permission@sei.cmu.edu for full terms.
