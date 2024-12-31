# Gitlab Agent

 The agent's flake leverages [Nix flakes](https://nixos.wiki/wiki/Flakes) and the [Crane](https://github.com/ipetkov/crane) library to streamline building, testing, packaging, and linting.


Three important parts of the framework implementation include:
* GitLab Resource Observer
    * Requires credentials for Gitlab in the form of a private token configured with read and write access for the API as well as credentials for authenticating with the given rabbitmq instance. The GitLab Observer is instantiated as a set of cooperating binaries, each handling a specific type of GitLab data.
* GitLab Message Consumer
    * The message consumer requires credentials for the rabbitmq instance as well and credentials for signing into a given neo4j graph database to publish information to. The consumer transforms specific GitLab data into domain-specified nodes and edges that can be used to tie CI/CD concepts to other domain knowledge.
* The Types Library
    * Contains implementation of the various GitLab types, as well as implementations  for serialization / deserialization.

All credentials, endpoints, and the like should be read in as environment variables,possibly from an environment file. There is an example an environment file in the gitlab agent [README](../../docs/README_gitlab.md) in the manual setup.

---

## Features

- **Build**: Ensures reproducible builds using Nix and Rust's `cargo` and `harkari` plugin
- **Lint**: Enforces code quality using Clippy and other tools.
- **Package**: Produces binary packages for distribution.
- **Containers**: Produces container images for each agent component.
- **Certificates**: Produces self signed certificates for mTLS using rabbitmq's [tls-gen](https://github.com/rabbitmq/tls-gen)

---

## Prerequisites

### Tools
 [Nix](https://nix.dev/) (Ensure flakes are enabled)
---

## Quick Start

Run static analysis checks
`nix flake check`

If you want to compile and package the agent's binaries, you can simply run from this directory
`nix build . --out-link "gitlab-agent"`

The packages will appear under a symlink named polar-gitlab-agent.

If we want to build one part of the agent over the others, or multiple parts , we can stack the nix build command like so.

`nix build .#gitlabConsumer .#consumerImage`
