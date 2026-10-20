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

The Scheduler Agent is responsible for managing the lifecycle of all schedules in Polar.
It consists of a single **RootSupervisor** actor that spawns the **ScheduleInfoProcessor**.

## Components

- **RootSupervisor**  
  - Initialises the Cassini client.  
  - Subscribes to `scheduler.in` (Git changes), `scheduler.adhoc` (agent registrations), and `events.#` (ephemeral triggers).  
  - Spawns the `ScheduleInfoProcessor` as a linked actor.

- **ScheduleInfoProcessor**  
  - Maintains an in‑memory cache of all schedules.  
  - Writes to / reads from the knowledge graph.  
  - Publishes schedule updates to agent‑specific topics (`agent.<id>.schedule`, `agent.type.<type>.schedule`).  
  - Registers event patterns and triggers ephemeral launches.

- **Git Watcher Observer** (separate binary)  
  - Watches the GitOps repo and publishes file changes to `scheduler.in`.

## Message Flow

- **Git change** → `scheduler.in` → `ScheduleInfoProcessor` updates graph and cache → publishes to agent topics.
- **Ad‑hoc agent registration** → `scheduler.adhoc` → `ScheduleInfoProcessor` creates/updates type schedule node → publishes current schedule to agent type topic.
- **Ephemeral event** → `events.#` → `ScheduleInfoProcessor` matches pattern → triggers launcher with instance config.

## Key Design Decisions

- The Scheduler **does not** start agents; it only provides configuration.  
- All inter‑component communication goes through Cassini topics.  
- The knowledge graph is the source of truth; the cache is only for performance.  
- Dhall is used for authoring schedules; evaluated to JSON at runtime.


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
