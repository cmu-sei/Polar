# Agent Configuration

Status: **partly built, partly direction.** Each section says which. Written 2026-10-07
from a design discussion, so that the reasoning is recorded and not only the code.

## 1. The goal

Make environment misconfiguration impossible to deploy, and let configuration change at
runtime safely, by keeping every requirement **in Rust code** and making everything else
(the deployment, the render, the schedules repo) answer to it.

Whoever owns a fact owns the rule about it:

- What the code needs of its configuration (which variables, what values, what must hold
  between them, which may change at runtime) is the code's. It is declared once, in Rust.
- What only the deployment knows (the pod's termination grace period against the shutdown
  budget) is the deployment's, and stays in the catalog tooling.

Nothing about the code's requirements is written a second time, so nothing drifts.

## 2. The declaration (built)

`polar-env` is where a variable is declared, once: its name, type, default, what it is for,
whether it is secret. Reading it, validating it and describing it all come from that one
declaration. A *section* groups a component's variables with the relations that must hold
between them.

A bad value fails naming the variable and what it is for; every problem is reported at once;
a secret is never shown; a typo is an error and never a quiet default.

## 3. Layers (direction)

A variable's effective value comes from the first of:

1. a **scheduled override**, applied at runtime (section 5);
2. the **environment**, which is the deployment's value;
3. the **default** in the declaration.

Each resolved value is reported with its source. This is the catalog's layering
(kind, workload, target) carried into runtime, with provenance.

**Mutability.** Each declaration says whether it is `Static` (the default) or `Dynamic`. Only
a Dynamic variable accepts a scheduled override and notifies subscribers. This is how the
code, not the repository, decides what may change while the agent runs. TLS paths, the broker
address, the capabilities and the health safety invariants stay Static.

## 4. Who holds it (direction)

- **The builder** takes one snapshot of the environment in `build()`, registers the sections
  that apply (the builder's own, each capability's components, and any an agent declares for
  itself), and checks them all before anything spawns.
- **The infrastructure supervisor** holds the result and hands each child its configuration
  already parsed. An actor never sees an environment, so its tests pass a configuration
  directly, with no process-wide state to lock.
- **Domain code** reads its own variables through `AgentResources`, and an undeclared read is
  refused, so the list of variables cannot exist apart from the code that reads it.
- **A controlled process environment.** The snapshot copies only declared variables, once. An
  undeclared variable that happens to be set, a secret included, never enters the agent's
  configuration, and later changes to the process environment are not seen. What the agent
  runs on is exactly what its code declared, which is a security property (nothing ambient
  leaks in or can be injected late) and a correctness one (the effective configuration is the
  same one that was checked).
- **A lint keeps direct reads out.** `clippy.toml` disallows `std::env::var`, `var_os`, `vars`,
  `vars_os`, `set_var` and `remove_var`, and a crate enforces it with
  `#![deny(clippy::disallowed_methods)]` in its root. A crate opts in when it has moved onto
  `polar-env` (today `polar-env`, `polar-health`, `polar-healthcheck`, `polar-agent-builder`, `polar-crypto` and `polar-init`),
  copying the file in; `ProcessEnv` is the one allowed read. The rest of the workspace is not
  enforced yet because about 90 direct reads in 36 files are still to be ported.

## 5. The ConfigurationActor and scheduled overrides (direction)

A per-agent actor under the infrastructure supervisor, the runtime shell around the pure
layered configuration. It is how other actors get their configuration and, later, how they are
told it changed.

The scheduler already exists in outline (`polar-scheduler`): a git observer publishes changes
to `scheduler.in`; a processor stores each `ScheduleNode` in the graph and publishes a
notification to `agent.<id>.schedule`; agents announce themselves on `scheduler.adhoc`. A
node's `config` is an untyped JSON value today. The declarations become the schema for it.

Properties the design needs:

- **The environment layer never depends on Cassini.** Every actor, including the Cassini
  client, needs its configuration before it starts. The remote layer attaches after the client
  registers, and an agent runs on the environment alone if the scheduler is unreachable.
- **Updates are transactional.** An override is parsed by the same declaration (a bad one is
  refused, naming the variable), and the section relations are re-checked on the candidate
  result. An agent cannot be put into a state its startup check would refuse. A refused update
  leaves the old value and is reported.
- **Notification is a message.** Subscribers receive a change on an output port and re-read.
  No closures run inside the actor.
- **Provenance.** Applied and rejected changes are recorded, with the schedule version.
- **Two roles, kept apart.** The scheduler agent publishes. The ConfigurationActor receives.

## 6. The credentials agent as a consumer (direction, with open questions)

The credentials agent will be the heaviest user of scheduled overrides:

- **Secrets** from the schedules repository, encrypted to the credentials agent's public
  escrow key, so the repository holds only ciphertext.
- **Policy** from the repository, applied to decisions the credentials agent makes.
- **Overrides** authored by a privileged user.

It is also the agent that cannot depend on remote configuration to start, because it serves
identity for everyone else. It starts on its environment alone and takes the rest when it
arrives. Open questions, not decisions:

- **Authenticity (direction from David, 2026-10-07).** The user who creates a config signs
  their work, and the receiver verifies the signature before applying anything. A writable topic
  is not authorization, and neither is a repository a push reached: the signature is what ties a
  change to a person. What exists to build on, found by reading the code (earlier chats could
  not be searched from here, so nothing recorded in them is assumed): users and agents already
  hold certificates issued by Polar's CA (cert-issuer), so the signer's identity can be the same
  one the rest of the system uses, and a signature can be checked back to the CA; `polar-policy`
  wraps Cedar, so "may this signer set this variable on this agent" is a policy question the
  system already knows how to ask; `cert-issuer` verifies OIDC token signatures (`oidc.rs`). What
  does not exist: any signing or verifying of configuration. `policy-config` only clones and
  pulls the policy repository, and `jwt-utils` states it verifies nothing. Still to decide: the
  signed form (a detached signature over a canonical encoding of the schedule, or signed git
  commits), and what the receiver checks (chain to the CA, signer authorized by policy for that
  variable and agent, version newer than the last applied).
- **Ordering.** Versions must be monotonic, so an old override cannot be replayed over a newer
  one.
- **Fail closed.** Before any policy has arrived, the credentials agent needs a safe default,
  which should be to refuse, not to allow.
- **Secrets as references elsewhere.** For agents that are not the credentials agent, secrets
  should travel as a name and key, not as ciphertext on a topic.

## 7. The schema export (direction)

`polar-config-check --describe --json` returns, as one JSON document, every variable with its type, default, purpose, secrecy and mutability. That one
export serves three uses:

- **Scaffolding a workload entry** at render time: start from a minimal entry, run the check,
  and the errors name each unset required variable; fill and repeat until clean.
- **Scaffolding a schedule**: the Dynamic variables are what a schedule may set.
- **Validating a schedule before it merges**, as CI on the schedules repository, against the
  declared Dynamic variables of its agent.

The generic checker knows the builder's and components' variables. An agent's own variables
and its minimum drain budget are only known to the agent, so the complete answer comes from
running a "check configuration and exit" mode in the agent's own image.

## 8. What exists today, and the order

Built: the declaration layer (`polar-env`), including the pure layered `Config` (a snapshot of
only the declared variables, the three layers with a source reported for each value, `Static`
and `Dynamic` mutability on every declaration, and all-or-nothing overrides that re-check every
section's relations); the health actor and probe on it; the builder's shared check
(`environment::check_environment`); the `polar-config-check` binary run by `resolve.nu check`;
and the catalog's own checks. The builder takes the one `Config` snapshot in `build()`
(`startup::prepare`, a pure function of an `Env`), with `with_environment` for an agent's own
sections; the infrastructure supervisor holds it and hands it out as `AgentResources::config`; the
health actor is handed its parsed `HealthConfig` and never sees an environment, and agents not yet
on the builder keep reading the process until they move.

Each capability brings the sections of the components it uses (`environment::sections_for`): the
Cassini client's and the graph's, declared in their own crates, so the builder judges them at
startup and the checker judges them for the environment a workload is given. Encryption brings the
crypto actor's (the service-account token path, beside the client's certificate path); the Kubernetes
API read brings none yet.

The Cassini client is started, and restarted, from the snapshot, and the graph controller is handed
its settings from it (the credentials consumer too: the builder is split into `configure()`, which reads and judges the
environment and spawns nothing, and `build()`, which spawns, so a domain supervisor can take its own
settings from the snapshot before it starts the actors that need them).

`polar-config-check --effective` shows what an environment resolves to, value by value with its
source, secrets hidden.

Next, in order: the ConfigurationActor shell, then the scheduler integration, then the credentials agent's use of
it. Every step is in `remaining-work.md`.

## 9. The agent's own interface (proposal, 2026-10-09)

For discussion; none of this is built. See `configuration-flow.md` for what exists and what is
missing.

**The wish.** An agent should be able to say what it is and what it needs, the way a good command
documents itself: `--help` for its purpose, and a way to list its environment as it was declared
with `polar-env` and hooked into the infrastructure, for people and for tools (the resolver, a
scaffolder, a documentation generator, a schedule editor).

**The difficulty.** Today an agent's declarations are made while it runs: the builder chain
(`AgentBuilder::new(..).with_graph_write()..`) executes inside a supervisor's `pre_start`, which
needs an actor system, and the agent's own variables are read with `std::env` where they are
used. Asking an agent about itself before it runs therefore has nothing to ask.

**Proposal: declare it as data, once, before the runtime.**

1. Each agent crate has one `const SPEC: AgentSpec`: its name, a one-line purpose, an optional long
   help text (a markdown file pulled in with `include_str!`, and a generated generic one when it
   is absent), its kind, the capabilities it requires, its shutdown budget, the sections of
   variables it adds to the builder's, and, as information, the topics it subscribes to and
   publishes.
2. The builder is constructed from the spec (`AgentBuilder::new(myself, &SPEC)`), and the `with_*`
   methods that declare things (graph write, encryption, self-test, the budget, `requires`) go
   away. What the agent says it is and what it does cannot differ, for the reason `polar-env`
   exists: one declaration, and everything else derived from it.
3. A shared `main` wrapper (`polar_agent_builder::run(&SPEC, start)`) handles the command line
   before any runtime or actor exists, as pure functions of the spec and the process environment:
   `--help`, `--describe [--json]`, `--check`, `--effective [--json]`. With no arguments it runs
   the agent, so a container's default command does not change. `polar-config-check` becomes the
   same functions over the builder's fixed composition, kept for the render step.
4. The agent's own variables are declared in sections with `polar-env` and read with
   `config.read(&VAR)`, as the graph's and the health actor's are, so they appear in `--describe`
   and `--effective`, and a variable the catalog sets that no section declares is an error.

**How tools get an agent's description without building and running every agent.** Options:

- Ask each binary at render time. Exact, but it needs every agent built before `resolve.nu check`
  can run.
- A committed contract file per agent, `infra/contracts/<agent>.json`, generated from the
  binary's `--describe --json` by a recipe, with a test that fails when the file is stale. The
  resolver reads the files. No build at render time, and a change to what an agent requires shows up
  in review as a diff. This is the recommendation.
- A registry crate that depends on every agent library. Cheap to ask, but it drags every agent's
  dependencies into the checker.

**What the resolver can then do.** Compare the catalog's `needs` with the code's `requires` (a
mismatch fails the render); refuse an `env` entry in a workload that the agent does not declare,
and a required variable nothing sets; show, in `explain`, the code's defaults for the variables the
manifest leaves unset, so the two views become one; and scaffold a workload entry from the
contract. The same file feeds the generated environment documentation and a schedule editor
(which variables are `Dynamic`).

**Things that need an answer:**

- Flags (`--describe`) or subcommands (`describe`)? Flags match `polar-config-check` and cannot be
  confused with a mode of the agent; subcommands read better and extend to `help <topic>`.
- Requirements that depend on configuration (a capability only when a feature is on): the spec
  holds the static superset and conditions are expressed as section relations, as the graph's
  client-certificate trio already is.
- Agents that do not use the builder (the Cassini broker, `polar-init`) could use the same wrapper
  with a spec that has no capabilities, but that is later.
- Where the long help text lives, and whether a missing one is an error in CI for builder agents
  or a generic fallback.
