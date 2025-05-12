Polar (OSS)

Copyright 2024 Carnegie Mellon University.

NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING
INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON
UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS
TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE
OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE
MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND
WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

Licensed under a MIT-style license, please see license.txt or contact
permission@sei.cmu.edu for full terms.

[DISTRIBUTION STATEMENT A] This material has been approved for public release
and unlimited distribution.  Please see Copyright notice for non-US
Government use and distribution.

This Software includes and/or makes use of Third-Party Software each subject
to its own license.

DM24-0470
# Getting Started in the Polar Workspace
This directory contains several of the agents that compose the polar framework.

See the README files for each component for details.

## Requirements

### Operating System
MacOS Monterrey or newer or modern Linux

Instructions written and tested on an Intel Mac running MacOS Sonoma (14.5) and Ubuntu 22.04 (LTS) on an Amazon EC2 instance (t3.2xlarge). 

### Hardware
- Multi-core CPU
- At least 8GB of RAM
- 25GB Free Storage

### Software
- [Rust](https://doc.rust-lang.org/cargo/getting-started/installation.html)
- GNU Make
- Git
- OpenSSL
- Sudo
- (Recommended, but not required, instructions written assuming Docker is installed) [Docker Engine](https://docs.docker.com/engine/install/) with [Docker Compose Plugin](https://docs.docker.com/compose/install/)

### A reminder - you don't *have* to install everything yourself

[This flake will give you a containerized development environment](../../dev-container/README.md) with everything needed to start development. 

## Building
The workspace leverages [Nix](https://nix.dev), [Nix flakes](https://nixos.wiki/wiki/Flakes), and the [Crane](https://github.com/ipetkov/crane) library to streamline the project's continuous integration and delivery practices. One will need to ensure the nix package manager is installed with flakes enabled to take advantage of it.

**NOTE:** This project specifcally targets x86-64-linux platforms *exclusively*, so we reccomend users either that they use a system or containerized environment that fits this. MacOS users in particular may be interested in running the [darwin-builder](https://github.com/NixOS/nixpkgs/blob/master/doc/packages/darwin-builder.section.md) locally as a virtual machine and delegating builds to it over ssh. Otherwise, using some other machine with nix installed as a [remote buider](https://nix.dev/manual/nix/2.18/advanced-topics/distributed-builds).

Running `nix build` from this directory will build all binaries specified by the Cargo.toml in this workspace by default.

If desired for testing, you can also use it to generate a self-signed client/server pair of certificates to facilitate the mTLS communications.

It is possible to build individual or multiple components at a time. Below are some examples of how.

```sh

# Build only the observer or consumer agents
nix build .#gitlabObserver -o gitlab-observer

nix build .#gitlabConsumer -o gitlab-consumer

# build all images as tar.gz archives to be loaded into a container runtime
nix build packages.x84_64.#cassiniImage .#observerImage .#consumerImage -o polar

# load the images into your runtime - for example, docker 

docker load polar
docker load polar-1
docker load polar-2

# Generate TLS Certificates
nix build .#tlsCerts -o certs

# Run static analysis, unit tests via derivations in checks on the workspace.
nix flake check

### For the impure - Build on a remote host
nix build \
--eval-system x86_64-linux \
--system x86_64-linux \
--builders "ssh://user@some-linux-host x86_64-linux" .#packages.x86_64-linux.default \
--show-trace \
--impure         

# If you've already configured a remote builder in your nix.conf, build all images available at time of writing + tlsCerts
nix build --eval-system x86_64-linux --show-trace --impure .#packages.x86_64-linux.observerImage .#packages.x86_64-linux.consumerImage .#packages.x86_64-linux.cassiniImage .#tlsCerts
# Impure remote checks
# nix flake check --eval-system x86_64-linux --builders "ssh://user@some-linux-host x86_64-linux" --show-trace --impure   
```

## Manual Steps

### mTlS Setup
If you can't or won't use `nix`, SSL files will need to be generated for the agents manually, or, you will have to provide your own certificates. This project formally used rabbitmq as it's message broker and one of its tools [tls-gen](https://github.com/rabbitmq/tls-gen), to create our self signed certificate files for testing. It is reccommended developers do so as well.

There are instructions in the `basic` directory in that repo for how to do so, but here are the basics:
   1. Clone the repo and change into the `basic` directory
   2. Run `make CN=polar` to generate the basic certificates.
   3. Copy the contents of the created `results` directory to somewhere the they can be easily found.
   For example, 
   `
      1. `mkdir $PROJECT_ROOT/conf/gitlab_compose/ssl`
      2. `cp results/* $PROJECT_ROOT/conf/certs`

## Adding/Removing Agents
As mentioned earlier, we use the crane library to handle various packaging and testing tasks. When adding or removing crates from the project, be sure to update the fileset in the `flake.nix` file, as it represents crane's understanding of our project workspace. 

When crane copies the project into it's build environment, it uses the list there to know which crates to take, if one is missing, cargo will **fail to build the workspace!**