# Polar Project Documentation

This document provides an overview of the Polar workspace structure, key files, and their purposes.

## Overview

Polar is a knowledge graph framework that collects infrastructure data and loads it into a graph database (currently Neo4j). It uses a pub/sub architecture with mutual TLS for secure communication between components. The project is built primarily in Rust and uses Nix for reproducible builds.

**Copyright:** 2024 Carnegie Mellon University  
**License:** MIT-style license  
**Primary Language:** Rust  
**Build System:** Nix + Flakes + Cargo

## Project Structure

```
/workspace
├── README.md                    # Main project overview
├── SECURITY.md                  # Security policy
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
├── docs/                        # Project documentation
├── scripts/                     # Utility scripts
├── var/                         # Runtime variable data (certs, volumes)
├── chart/                       # Helm charts for Kubernetes deployment
├── examples/                    # Example applications
├── .devcontainer/              # VSCode Dev Container configuration
├── .direnv/                    # direnv configuration
├── conf/                       # Configuration files
├── client/                     # Client-side components
├── server/                     # Server-side components
├── dev/                        # Development resources
├── dev-container/             # Dev container specifications
├── pipeline-out/              # Pipeline output artifacts
├── result-agent-container     # Symlink to agent container image
├── result-tlsCerts            # Symlink to TLS certificates
└── ca_certificates/           # CA certificate storage
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
- **config-ops/** - Configuration operations
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
- **policy-config/** - Policy configuration
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

Dhall configurations for generating Kubernetes manifests with GitOps workflows.

#### `src/flake/` - Nix Build Configurations

Nix-related build files including:
- Agent container definitions
- TLS certificate generation
- CI/CD pipeline configurations

#### `src/git-hooks/` - Git Hooks

Git pre-commit hooks for static analysis.

### `docs/` - Documentation

Comprehensive documentation including architecture and operational guides.

#### `docs/architecture/` - Architecture Documentation

- `Polar.md` - Main architecture documentation
- `attestation_and_issuance.md` - Secure attestation protocol
- `cassini_testing.md` - Testing documentation for Cassini
- `git/` - Git-specific architecture docs
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
- `ssl/` - SSL/TLS certificates (CA, client, server)
- `tls-gen/` - TLS certificate generation data

### `chart/` - Helm Charts

Helm chart configurations for Kubernetes deployments:
- `Chart.yaml` - Chart metadata
- `charts/` - Sub-charts
- `values.yaml` - Default values

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

# Generate TLS certificates
nix build .#tlsCerts -o certs

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

Deployments use Dhall configurations rendered to Kubernetes manifests via Helm:

```bash
# Generate manifests
sh scripts/render-manifests.sh src/deploy/<environment> <output-dir>
```

### GitOps Workflow

- Manifests are version-controlled and immutable
- Flux watches the Git repository for changes
- Automated continuous deployment
- Rollback capability to previous known-good versions

### Agent Deployment Requirements

Each agent requires:
- mTLS certificates (certificate, private key, CA cert)
- Environment variables for credentials
- Access to the Cassini message broker
- Network access to target systems (GitLab, Jira, Kubernetes API)

## Security

- **mTLS**: All service communication uses mutual TLS
- **Certificate Management**: Certificates generated via Nix or tls-gen
- **Attestation Protocol**: Secure short-lived certificate issuance for containers
- **Rate Limiting**: Protection against excessive requests
- **Logging**: Comprehensive logging with anomaly detection
- **Credential Provider Abstraction**: Clean separation of authentication logic

### Attestation Protocol

The system implements a secure attestation protocol for containerized services:
- Challenge-response mechanism to prevent replay attacks
- Ephemeral secrets bound to running instances
- HMAC signatures for request integrity
- Runtime introspection via SSH to verify container existence
- Rate limiting and anomaly detection

## Resources

- **Polar Logo**: `docs/Polar-Logo.png`
- **Documentation**: See individual README.md files in subdirectories
- **Architecture**: `docs/architecture/` directory
- **Agent Docs**: `src/agents/*/README.md`
- **Blog**: "Polar: Improving DevSecOps Observability" at https://insights.sei.cmu.edu/blog/polar-improving-devsecops-observability/

## License

Copyright 2024 Carnegie Mellon University.  
Licensed under a MIT-style license.  
See `license.txt` for full terms.

## Distribution Statement

[DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited distribution.
