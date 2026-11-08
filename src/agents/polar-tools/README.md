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

# polar-tools

The command-line tools you run yourself when you use Polar, built as **static
binaries** and shipped in one small image, so that installing them is a copy, not a
compile.

## Why it exists

Bringing a cluster up used to mean compiling `polar-admin-grant` and `polar-sign`
from source on the machine doing the install, which needs a Rust toolchain and takes
minutes. It also left non-Rust applications with no way to use `cassini-client`. This
image fixes both: the tools are built once per architecture, and a user copies them
out.

Every binary here is linked statically (musl, with OpenSSL built in), always. A
copy therefore runs on any Linux of the same architecture, whatever its C library
and whether or not Nix is installed. Nothing is ever built dynamically, because a
dynamically linked Nix build would point at `/nix/store` paths that do not exist
anywhere else.

## What is in it

| Tool | What it is for |
|---|---|
| `polar-sign` | sign, seal and push configuration documents to the schedules repository |
| `polar-admin-grant` | create and claim accounts, grant permissions, bootstrap the cluster CA |
| `cassini-c2` | ask the Cassini broker to shut down |
| `cassini-client` | the broker's command-line client, for applications that are not written in Rust |
| `user-cert-fetcher` | self-service user certificates |
| `cert-client`, `cert-issuer-setup` | the local PKI |

They sit as ordinary files in `/opt/polar/bin` inside the image. The image has no
shell and nothing to run; it is a container only so that it travels through
registries like every other Polar image, for every architecture the flake builds.

## Using it

Copy the tools into a directory of your choice and put that directory on your `PATH`.
From the repository root:

```bash
nu infra/tools.nu install ~/.local/bin
```

That reads `polar-tools:latest` from the registry in `POLAR_REGISTRY` (default
`localhost:5000`). To read it from somewhere else, give `skopeo`'s reference:

```bash
nu infra/tools.nu install ~/.local/bin --from docker://registry.example.org/polar-tools:latest
nu infra/tools.nu install ~/.local/bin --from docker-archive:result/polar-tools
```

It needs `skopeo` and `tar`, and no container runtime. Without the script, the same
thing by hand:

```bash
skopeo copy docker://registry.example.org/polar-tools:latest dir:/tmp/polar-tools
# each file in /tmp/polar-tools named by a digest is a layer; extract the tools from them:
for layer in /tmp/polar-tools/[0-9a-f]*; do tar -xf "$layer" -C /tmp --wildcards '*opt/polar/bin/*' 2>/dev/null; done
cp /tmp/opt/polar/bin/* ~/.local/bin/
```

Two things to know:

- `polar-sign` runs `git`, so `git` must be on your `PATH`.
- A tool is for the architecture of the image you copied it from. Use the `polar-tools`
  image that matches the machine you run it on.

The dev container (`just dev-container`) has these tools on its `PATH` already.

## How scripts find them

`infra/up.nu`, `infra/cassini-shutdown.nu` and the admin recipes in the `Justfile`
use the tool on your `PATH` when it is there. When it is not, they run it with
`cargo run` from this checkout, which is what a developer with a Rust toolchain
wants. `infra/tools.nu` holds that rule (`tools command <name>`).

## Building it

The image and the plain binaries are flake outputs, per architecture:

```bash
nix build .#polarPkgs.polarTools.tools   # $out/bin/<tool>, for a developer's PATH
nix build .#polarPkgs.polarTools.image   # the image: a docker archive
just images-push polar-tools             # build it and push it to the registry
```

`package.nix` in this directory defines both. To add a tool, add its package to the
`packages` list there. The check that it really is static is `ldd <binary>`, which must
say `statically linked`.
