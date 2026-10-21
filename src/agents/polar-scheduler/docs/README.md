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

# Scheduler Agent – Design Overview

The scheduler is two agents. The **observer** (`polar-scheduler-observer`) keeps a checkout of the `polar-schedules`
repository on the Polar git server and publishes each file that changes on `scheduler.in`. The **processor**
(`polar-scheduler`) handles what it publishes: it writes each signed document to the graph and forwards it, untouched, to
the agent it is addressed to (`agent.<agent>.configuration`), which verifies it. Neither verifies a document; the receiver
does.

## Message flow

- **A push to `polar-schedules`** → `polar-git-repo-changed` → the observer syncs its checkout → each changed file on
  `scheduler.in`.
- **A signed document** (`scheduler.in`) → the processor writes a `(:Schedule:Configuration {id})` node and, when its
  version is new, publishes a `ConfigurationNotification` on `agent.<agent>.configuration`. A document it cannot route is
  written and goes nowhere.
- **A file that is not a signed document** → logged and ignored. The unsigned schedule path (`permanent`, `adhoc` and
  `ephemeral` schedules, `scheduler.adhoc`, `agent.<id>.schedule`, `agent.type.<type>.schedule`) was removed on
  2026-10-21 (`docs/architecture/policy-enforcement.md`, "Decisions").
- **An agent's outcome** (`polar-configuration-outcome`) → the processor writes a `ConfigurationOutcome` node linked to
  the document.
- **An agent asking for a replay** (`polar-configuration-replay`) → the observer publishes the documents it holds for that
  agent on its configuration topic.

The sequences are drawn in `docs/architecture/sequences/general/signed_configuration_delivery.mmd`, and the kinds of
document in `docs/architecture/scheduling-kinds.md`.

## Key design decisions

- The scheduler does not start agents; it only delivers their configuration.
- All communication between components goes through Cassini topics.
- The knowledge graph keeps every document received, and every outcome reported.
- Documents are authored and signed with `polar-sign` and evaluated to JSON in the repository.


# Integration Testing

The scheduler is tested in a cluster, with the rest of Polar. On `local-minimal`, `nu infra/up.nu local-minimal --dev`
deploys the observer and the processor with the Polar git server they watch, and seeds the `polar-schedules` repository
with its first signed policy (the seed step). To try a change to either agent:

```bash
nu infra/images.nu sync scheduler-observer scheduler-processor --target local-minimal   # build both, push them to the cluster's registry
nu infra/up.nu local-minimal                                                           # restarts the Jobs whose image changed, and only those
kubectl -n polar logs -l job-name=polar-scheduler-observer -f
```

The earlier local procedure (Neo4j, Jaeger and Cassini from `scripts/dev_stack.sh`, the agents with `cargo run`, a
`file://` schedules repository) was retired on 2026-10-20 with the dev shell's static certificates it depended on; it is
in the history before then.

## Environment Variables Summary

| Variable | Purpose | Example |
|----------|---------|---------|
| `POLAR_SCHEDULER_REMOTE_URL` | Git remote URL (required for tests) | `file:///tmp/test-repo` or `https://github.com/...` |
| `POLAR_SCHEDULER_LOCAL_PATH` | Local directory for the working copy | `/tmp/polar-schedules-test` |
| `POLAR_SCHEDULER_SYNC_INTERVAL` | Seconds between `git pull` (optional) | `30` |
| `POLAR_SCHEDULER_GIT_USERNAME` | HTTPS username (if needed) | `user` |
| `POLAR_SCHEDULER_GIT_PASSWORD` | HTTPS password or token | `token` |

These variables are read by the observer; the processor only needs the standard Neo4j environment variables (`GRAPH_ENDPOINT`, `GRAPH_USER`, `GRAPH_PASSWORD`, `GRAPH_DB`).

## Container Images

Both scheduler components are available as minimal container images built with Nix. To build them:

```bash
# Build the processor image
nix build .#polarPkgs.scheduler.processorImage -o processor-image

# Build the observer image
nix build .#polarPkgs.scheduler.observerImage -o observer-image
```

The resulting images will be in `./processor-image` and `./observer-image` as compressed tarballs (Docker archives). You can load them into Docker/Podman with:

```bash
docker load < processor-image
docker load < observer-image
```

### Verify the images

Run the containers locally to ensure they start correctly (you'll need to supply the required environment variables):

```bash
# For the processor (Neo4j must be reachable)
docker run --rm -e GRAPH_ENDPOINT=bolt://host.docker.internal:7687 ... polar-scheduler-processor:latest

# For the observer (needs Git remote URL and local path)
docker run --rm -e POLAR_SCHEDULER_REMOTE_URL=... -e POLAR_SCHEDULER_LOCAL_PATH=/data ... polar-scheduler-observer:latest
```

All configuration is done via environment variables at runtime.
