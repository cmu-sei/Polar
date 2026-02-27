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
