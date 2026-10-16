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

# Polar Project Documentation

This document provides an overview of the Polar workspace structure, key files, and their purposes.

## Overview

Polar is a knowledge graph framework that collects infrastructure data and loads it into a graph database (currently Neo4j). It uses a pub/sub architecture with mutual TLS for secure communication between components. The project is built primarily in Rust and uses Nix for reproducible builds.

**Copyright:** 2026 Carnegie Mellon University  
**License:** MIT (SEI)-style license  
**Primary Language:** Rust  
**Build System:** Nix + Flakes + Cargo

## Project Structure

```
/workspace
├── README.md                    # Main project overview
├── SECURITY.md                  # Security policy
├── Justfile                     # Build, cluster and admin-flow recipes
├── maskfile.md                  # shortcuts for common dev operations
├── flake.nix                    # Nix flake for reproducible builds
├── flake.lock                   # Locked Nix dependencies
├── example.env                  # Example environment configuration
├── my.env                       # Current environment configuration
├── .envrc                       # Environment configuration for direnv
├── .gitignore                   # Git ignore rules
├── .gitlab-ci.yml              # GitLab CI/CD configuration
├── settings.json                # VSCode settings
├── DOCUMENTATION.md             # This file
├── pi/                          # (Ignored per user request)
├── src/                         # Source code and agents
├── infra/                       # Layered Kubernetes deployment (render.nu, targets, charts)
├── docs/                        # Project documentation
├── scripts/                     # Utility scripts
├── var/                         # Runtime variable data (volumes)
├── examples/                    # Example applications
├── .devcontainer/              # VSCode Dev Container configuration
├── .direnv/                    # direnv configuration
├── conf/                       # Configuration files
├── client/                     # Client-side components
├── server/                     # Server-side components
├── dev/                        # Development resources
├── dev-container/             # Dev container specifications
├── pipeline-out/              # Pipeline output artifacts
└── result-agent-container     # Symlink to agent container image
```

## Key Files and Directories

### `src/` - Source Code and Agents

The core of the Polar framework, containing Rust agents that implement the pub/sub architecture.

#### `src/agents/` - Agent Workspace

Contains the main Rust workspace with multiple agents:

- **broker/** - Message broker implementation (Cassini)
- **cassini/** - Core messaging infrastructure
  - `broker/` - Message broker server
  - `client/` - Client library for connecting to the broker
  - `test/` - Test harness for broker validation
- **cert-issuer/** - Certificate authority service: issues short-lived workload certificates from Kubernetes or IdP tokens, and signs account certificates for the credentials-agent
- **config-ops/** - Configuration operations
- **credentials-agent/** - Account and credential management: event-sourced, hash-chained ledger; HTTP observer; consumer that verifies signed admin actions; `polar-admin-grant` admin CLI
- **git/** - Git repository agents
- **gitlab/** - GitLab integration agents
  - Observer - Collects GitLab data
  - Consumer - Transforms GitLab data into graph nodes
  - config/ - Configuration utilities
  - common/ - Shared code
- **jira/** - Jira integration agents
- **kubernetes/** - Kubernetes agents
- **lib/** - Common utility library
- **logger/** - Logging utilities
- **openapi/** - OpenAPI specification agents
- **polar-scheduler/** - Scheduler agent for managing schedules
  - docs/ - Scheduler design documentation
- **provenance/** - Provenance tracking agents
  - linker/ - Links artifacts across systems
  - resolver/ - Resolves artifact references
  - provenance-common/ - Shared provenance code
- **Cargo.toml** - Workspace configuration
- **Cargo.lock** - Locked dependencies
- **workspace.nix** - Nix build configuration

#### `src/conf/` - Configuration Files

Contains configuration for running a local stack with Docker Compose.

#### `src/deploy/` - Kubernetes Deployments

Older Dhall configurations for generating Kubernetes manifests with GitOps workflows. The current deployment pipeline is `infra/`.

#### `src/flake/` - Nix Build Configurations

Nix-related build files including:
- Agent container definitions
- CI/CD pipeline configurations

#### `src/git-hooks/` - Git Hooks

Git pre-commit hooks for static analysis.

### `infra/` - Deployment Pipeline

Layered Dhall charts rendered to Kubernetes manifests and applied in dependency order by `nu infra/render.nu <target> --apply`.

- `layers/` - Charts grouped by layer (for example `2-services/` holds cert-issuer, cassini, neo4j and the agents)
- `targets/<name>/` - Deployment profiles (`local-minimal`, ...): `target.nu` lists what a target deploys, `overrides.dhall` holds per-deployment values, `secrets.sops.yaml` holds encrypted cluster secrets
- `schema/` - Shared Dhall types and constants (namespaces, hostnames, certificate paths)

See `README.md` for the full deployment walkthrough.

### `docs/` - Documentation

Comprehensive documentation including architecture and operational guides.

#### `docs/architecture/` - Architecture Documentation

- `Polar.md` - Main architecture documentation
- `attestation_and_issuance.md` - Secure attestation protocol
- `cassini_testing.md` - Testing documentation for Cassini
- `git/` - Git-specific architecture docs
- `identity-and-bootstrap-update.md` - Workload identity, cert-issuer, accounts, claims and admin bootstrap
- `kubernetes/` - Kubernetes architecture docs
- `polar-logical-architecture.md` - Logical architecture model
- `secrets-management.md` - Secrets management documentation
- `sequences/` - Sequence diagrams

#### `docs/devops.md` - DevOps Guide

Documentation for the CI/CD pipeline using Nix and GitLab CI.

### `scripts/` - Utility Scripts

- `dev_stack.sh` - Starts local development stack
- `gitlab-ci.sh` - Local GitLab CI simulation
- `render-manifests.sh` - Renders Kubernetes manifests from Dhall
- `setup-neo4j.sh` - Neo4j setup script
- `static-tools.sh` - Static analysis tools runner
- `backup/` - Backup utilities
- `loaders/` - Data loader scripts

### `var/` - Variable Data

Runtime data including:
- `neo4j_volumes/` - Neo4j database volumes

### `examples/` - Example Applications

Example applications demonstrating Polar usage:
- `todo-app/` - Todo application example

### `pi/` - (Ignored)

The `pi` directory contains documentation and examples for the Pi agent framework and is not part of this workspace's documentation.

## Key Components

### Cassini - Message Broker

A resilient message broker built using Rust's Ractor framework. It handles service-to-service messaging with mutual TLS (mTLS) security.

**Features:**
- Actor-based architecture
- Message routing to multiple subscribers
- Session management with unique session IDs
- Secure mTLS communication

### Agent Architecture

Polar uses a modular agent system:

1. **Observer Agents** - Collect data from external systems (GitLab, Kubernetes, Jira, etc.)
2. **Consumer Agents** - Transform collected data into graph nodes and edges
3. **Scheduler Agent** - Manages scheduling and policy application
4. **Configuration Agent** - Serves runtime configurations to agents

All communication happens via the Cassini message broker using mTLS.

### Identity and Certificates

Every certificate in a deployment comes from the cluster's own CA, held by `cert-issuer`:

- **Workloads** get short-lived client or server certificates from the `polar-init` init container, which presents the pod's projected ServiceAccount token to `cert-issuer`. The certificate's identity is `<serviceaccount>.<namespace>.serviceaccount.cluster.local`.
- **People** use a personal client certificate (ECDSA P-256, one year) tied to an account that an admin provisioned. The credentials-agent verifies the claim and has `cert-issuer` sign the certificate; the private key is generated on the holder's machine.
- **Credentials-agent** keeps accounts, certificates, grants and revocations in an event-sourced, hash-chained ledger. Admin actions are signed with the author's certificate key and verified by the consumer, which is the only gate.
- **Observer** (`credentials-agent-observer`) is an HTTP-to-Cassini relay that exposes `/issue`, `/identity`, `/account` and `/admin-action`. It decides nothing.
- **`polar-admin-grant`** is the admin CLI: `bootstrap-ca`, `bootstrap`, `claim`, and signed admin actions such as `provision-account`. The Justfile wraps it as `admin-bootstrap`, `admin-provision`, `admin-claim` and `user-cert-install`.

See `docs/architecture/identity-and-bootstrap-update.md` for the design and `README.md` for the walkthrough.

### Provenance Tracking

The Provenance Agent traces the origin and lifecycle of software artifacts across:
- Source control (Git)
- CI/CD pipelines
- Container registries
- Kubernetes deployments

Uses `ImageReference` as a canonical reference for linking artifacts across systems.

### Git Agents

A three-agent system for Git repository introspection:
1. **Scheduler Agent** - Applies policy and manages scheduling
2. **Git Repository Observer** - Clones and analyzes repositories
3. **Git Repository Processor** - Persists data to the knowledge graph

### OpenAPI Agents

Agents for discovering, fetching, and consuming OpenAPI specifications:
- Automatic discovery of OpenAPI endpoints
- Spec validation against OpenAPI schema
- Normalization into internal representation
- Change tracking and diffing

## Build and Development

### Prerequisites

- **Nix** package manager with flakes enabled
- **Rust** (via Nix or rustup)
- **Docker/Podman** (for local development stack)
- **Git**

### Building

```bash
# Build all components
nix build

# Build specific components
nix build .#polarPkgs.gitlabObserver.observer -o gitlab-observer
nix build .#polarPkgs.gitlabConsumer.consumer -o gitlab-consumer
```

### Running Tests

```bash
# Run unit tests
cargo test --package polar -- --nocapture

# Some integration tests require Docker for testcontainers
```

### Development Setup

```bash
# Start local development stack
./scripts/dev_stack.sh

# Run static analysis
./scripts/static-tools.sh
```

### Development Container

The project includes a VSCode Dev Container configuration in `.devcontainer/`:
- Uses `daveman1010220/polar-dev` as base image
- Custom user creation via `create-user.sh`
- Configurable UID/GID via environment variables

## Configuration

Environment variables are used for configuration. See `example.env` for required variables including:
- Graph database endpoint and credentials
- Message broker address and TLS certificates
- GitLab/Jira API tokens
- Kubernetes cluster access

In a cluster, the `TLS_*` variables point at files that `polar-init` writes from certificates issued by `cert-issuer`; there is no static, pre-generated certificate set.

### Key Environment Variables

| Variable | Purpose |
|----------|---------|
| `GRAPH_ENDPOINT` | Neo4j database connection string |
| `BROKER_ADDR` | Cassini message broker address |
| `CASSINI_SERVER_NAME` | Server identity for mTLS |
| `GRAPH_USER` / `GRAPH_PASSWORD` | Neo4j credentials |
| `GRAPH_DB` | Neo4j database name |
| `TLS_CLIENT_KEY` | Client private key path |
| `TLS_KEY_PASSWORD` | Key password (if any) |
| `TLS_CA_CERT` | CA certificate path |
| `GITLAB_TOKEN` | GitLab API token |
| `GITLAB_ENDPOINT` | GitLab GraphQL endpoint |
| `JIRA_TOKEN` | Jira API token |
| `JIRA_URL` | Jira REST API URL |
| `KUBECONFIG` | Kubernetes config file path |

## Deployment

### Kubernetes

Layered Dhall charts rendered to Kubernetes manifests by `nu infra/render.nu <target>` and brought up, in dependency order and with the CA, secrets and admin flow, by `nu infra/up.nu <target>`.

```bash
kubectl apply -f manifests/namespaces.yaml
(cd src/agents && cargo run -p polar-admin-grant -- bootstrap-ca)
nu infra/render.nu local-minimal --apply
```

`bootstrap-ca` creates the cluster's CA once and needs a Kubernetes admin credential. After the pods are running, `just port-forwards-up` exposes the services locally, and the admin flow (`just admin-bootstrap`, `admin-provision`, `admin-claim`, `user-cert-install`) issues a personal certificate for Neo4j Browser and the admin tools. `README.md` has the full walkthrough.

### GitOps Workflow

- Manifests are version-controlled and immutable
- Flux watches the Git repository for changes
- Automated continuous deployment
- Rollback capability to previous known-good versions

### Agent Deployment Requirements

Each agent requires:
- mTLS certificates (certificate, private key, CA cert), issued automatically by `cert-issuer` through the `polar-init` init container
- Environment variables for credentials
- Access to the Cassini message broker
- Network access to target systems (GitLab, Jira, Kubernetes API)

## Security

- **mTLS**: All service communication uses mutual TLS
- **Certificate Management**: Certificates are issued by `cert-issuer` from the cluster's own CA; workload certificates are short-lived and reissued on every pod start
- **Workload Attestation**: Pods prove identity with projected ServiceAccount tokens
- **Rate Limiting**: Protection against excessive requests
- **Logging**: Comprehensive logging with anomaly detection; bearer tokens and CSRs are not written to logs
- **Credential Provider Abstraction**: Clean separation of authentication logic

### Identity and Issuance

- Workloads present a projected ServiceAccount token; `cert-issuer` validates it against its trusted issuers (the Kubernetes API and, optionally, an external IdP such as Keycloak) and signs a certificate whose identity must match the CSR byte for byte
- Neo4j requires a client certificate on both Bolt and HTTPS and trusts only the cluster CA for client authentication
- People hold personal client certificates (ECDSA P-256, one year). A certificate is never self-renewed: replacing one always involves an admin
- Admin actions (provisioning, grants, revocations) are signed with the author's certificate key and verified by the credentials-agent consumer, which looks up the author's permissions in its own ledger; the transport and the broker are not trusted for authorization
- The one-time bootstrap credential can provision exactly one account, after which it is spent

## Resources

- **Polar Logo**: `docs/Polar-Logo.png`
- **Documentation**: See individual README.md files in subdirectories
- **Architecture**: `docs/architecture/` directory
- **Agent Docs**: `src/agents/*/README.md`
- **Blog**: "Polar: Improving DevSecOps Observability" at https://insights.sei.cmu.edu/blog/polar-improving-devsecops-observability/

## License

Copyright 2026 Carnegie Mellon University.  
Licensed under a MIT (SEI)-style license.  
See `license.txt` for full terms.

## Distribution Statement

[DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited distribution.
