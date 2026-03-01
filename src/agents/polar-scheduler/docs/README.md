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

The scheduler includes a set of integration tests that verify end‑to‑end functionality. The tests
use a real Neo4j database, Jaeger for tracing, and a local Git repository for schedule storage.
Gitea is no longer used; instead, a local Git repository (with a `file://` remote) simulates the
remote GitOps repo.

## Prerequisites

- Docker or Podman (for Neo4j and Jaeger containers)
- A working Polar development environment (`nix develop` or `direnv`)
- A local Git repository with test Dhall schedules (see below)

## Step‑by‑Step Test Procedure

1. **Create a test Git repository**  
   ```bash
   TEST_REPO=$(mktemp -d)
   cd "$TEST_REPO"
   git init
   mkdir -p permanent
   cat > permanent/test.dhall << 'EOF'
   ... (sample schedule content) ...
   EOF
   git add permanent/test.dhall
   git commit -m "feat(test): add test schedule"
   git remote add origin "file://$TEST_REPO"
   ```
   Replace the sample schedule with the one from the project’s `schedules/permanent` directory.

2. **Start the infrastructure stack**  
   ```bash
   cd src/agents/polar-scheduler
   export POLAR_SCHEDULER_REMOTE_URL="file://$TEST_REPO"
   ./scripts/dev_stack.sh up
   ```
   This launches Neo4j, Jaeger, and the Cassini broker.

3. **Run the scheduler observer** (in a separate terminal)  
   ```bash
   cd src/agents/polar-scheduler/observer
   export POLAR_SCHEDULER_LOCAL_PATH="/tmp/polar-schedules-test"
   export POLAR_SCHEDULER_REMOTE_URL="file://$TEST_REPO"
   export POLAR_SCHEDULER_SYNC_INTERVAL="30"
   cargo run
   ```
   The observer clones the repo (if needed) and starts watching for changes.

4. **Run the scheduler processor** (in another terminal)  
   ```bash
   cd src/agents/polar-scheduler/processor
   cargo run
   ```
   The processor connects to Neo4j and subscribes to `scheduler.in`.

5. **Run a mock agent** (optional, to receive schedule updates)  
   ```bash
   cd src/agents/polar-scheduler/tests/mock-agent
   cargo run
   ```
   The mock agent subscribes to `agent.test-permanent.schedule` and logs any received schedule JSON.

6. **Trigger a schedule change**  
   Modify the Dhall file in the test repo, commit, and (if using a local repo) push is not needed; the observer’s filesystem watcher detects the change directly because it watches the local working copy.
   ```bash
   cd "$TEST_REPO"
   # Edit permanent/test.dhall (e.g., bump version to 2)
   sed -i 's/version = 1/version = 2/' permanent/test.dhall
   git commit -am "feat(test): update schedule"
   ```
   The observer publishes a `GitScheduleChange::Update`, the processor updates the graph, and the mock agent (if running) logs the new schedule.

7. **Verify in Neo4j**  
   Open http://localhost:7474 (credentials `neo4j`/`somepassword`) and run:
   ```cypher
   MATCH (s:Schedule:Permanent {agent_id: 'test-permanent'}) RETURN s.schedule
   ```
   The returned JSON should reflect the latest version.

8. **Clean up**  
   Stop all running processes (Ctrl+C in each terminal). Then bring down the stack:
   ```bash
   cd src/agents/polar-scheduler
   ./scripts/dev_stack.sh down
   ```
   Optionally remove the temporary test repo:
   ```bash
   rm -rf "$TEST_REPO"
   ```

## Environment Variables Summary

| Variable | Purpose | Example |
|----------|---------|---------|
| `POLAR_SCHEDULER_REMOTE_URL` | Git remote URL (required for tests) | `file:///tmp/test-repo` or `https://github.com/...` |
| `POLAR_SCHEDULER_LOCAL_PATH` | Local directory for the working copy | `/tmp/polar-schedules-test` |
| `POLAR_SCHEDULER_SYNC_INTERVAL` | Seconds between `git pull` (optional) | `30` |
| `POLAR_SCHEDULER_GIT_USERNAME` | HTTPS username (if needed) | `user` |
| `POLAR_SCHEDULER_GIT_PASSWORD` | HTTPS password or token | `token` |

These variables are read by the observer; the processor only needs the standard Neo4j environment variables (`GRAPH_ENDPOINT`, `GRAPH_USER`, `GRAPH_PASSWORD`, `GRAPH_DB`).
