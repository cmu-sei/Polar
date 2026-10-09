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

`<agent> --scaffold` returns, as one JSON document, every variable with its type, default, purpose, secrecy and mutability. That one
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
section's relations); the health actor and probe on it; the builder's startup check
(`startup::prepare`); the agent's own `--check`, run by `resolve.nu check`; and the catalog's own
checks. (The separate `polar-config-check` program is retired.) The builder takes the one `Config` snapshot in `build()`
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

`<agent> --scaffold --config FILE` shows what a document resolves to, value by value with its
source, secrets hidden.

Next, in order: the ConfigurationActor shell, then the scheduler integration, then the credentials agent's use of
it. Every step is in `remaining-work.md`.

## 9. The agent's own interface (decided 2026-10-09, not yet built)

See `configuration-flow.md` for what exists today and what is missing. This section is the design
that closes the gaps there, agreed with David on 2026-10-09 after a first proposal (a committed
contract file per agent, and `needs` kept in the catalog) was rejected: both were copies to keep in
sync with the code.

### 9.1 The principle

The code is the source of truth. An agent says what it needs once, in code, by registering it with
`polar-env` and with the builder; everything else is derived from that, by the binary itself.
Nothing is kept in step by hand, because nobody writes a second copy: they ask the binary for a
**scaffold**, fill it in, and the agent reads the filled-in document at runtime.

### 9.2 What the code declares: a trait every agent implements

(Shape suggested by David on 2026-10-09: make the form of what we require of an agent the form of
a trait, so implementing it is hooking into the infrastructure.)

```rust
pub trait PolarAgent: Sized + 'static {
    const NAME: &'static str;
    const PURPOSE: &'static str;                       // one line, required
    const NEEDS: &'static [Capability];                // what it asks of its deployment
    const SHUTDOWN_BUDGET: Duration;                   // the least it needs to drain
    const SECTIONS: &'static [&'static Section] = &[]; // its own variables, declared with polar-env
    const HELP: Option<&'static str> = None;           // long help, e.g. include_str!("../HELP.md")

    /// The agent proper. It is handed the configured infrastructure and nothing else, so the
    /// builder is the only way to get a router, a Cassini client or a graph.
    fn start(agent: ConfiguredAgent) -> impl Future<Output = Result<(), StartError>> + Send;
}

// main.rs
fn main() -> ExitCode { polar_agent_builder::run::<KubeConsumer>() }
```

Why a trait and not a value built by a function:

- **The compiler enforces the form.** An agent that does not say what it needs, its budget or
  its purpose does not compile; the optional parts have defaults. There is no way to forget
  `NEEDS`.
- **Nothing runs to ask.** Associated constants and functions are available before any runtime or
  actor exists, which is what `--help`, `--scaffold` and `--check` need. `run::<A>()` reads them
  first, answers the command line, and only for a plain run starts the runtime.
- **The builder cannot be bypassed.** `start` receives the `ConfiguredAgent` the builder made from
  these very constants, and `run` is the only caller. What the agent declared and what it was
  given cannot differ, and an agent cannot build its own infrastructure on the side.
- **One more check for free.** A test macro, `polar_agent_tests!(KubeConsumer)`, run once in each
  agent crate, checks the declaration: the purpose is not empty, the budget is at least one second,
  every section's defaults parse (`Section::defaults_problems`), and no variable is declared twice.

The agent's own variables are declared in sections with `polar-env` and read with
`config.read(&VAR)`, as the graph's and the health actor's are. The existing
`disallowed_methods` lint, which bans `std::env::var` in the migrated crates, is extended to the
Jira and Kubernetes agents, so an agent cannot read a variable it did not declare: it does not
compile.

This is not a manifest next to the code. It is the one statement of the code's needs, in the
language the code is written in, and the builder is built from it. Link-time registration of
sections alone would not do: every agent links the graph crate, so every scaffold would list the
graph's variables as required. `NEEDS` is what says which sections apply.

### 9.3 The command line

A shared `main` wrapper, `polar_agent_builder::run(declare(), start)`, answers the command line
before any runtime or actor exists, as pure functions of the declaration and a configuration
document. With no arguments it runs the agent, so a container's default command does not change.

| Flag | For | What it does |
| --- | --- | --- |
| `--help` | people | the agent's purpose and long help text (generated and generic when the agent has none), the capabilities it needs, its variables in brief, and these flags |
| `--scaffold` | tools and people | prints the configuration document as JSON: the schema with every variable's type, default, secrecy, mutability and documentation, the capabilities required, the minimum shutdown budget, and each value empty. With `--config FILE` it prints the same document filled from FILE, with where each value came from and every problem found |
| `--check --config FILE` | the resolver, CI | the judgement the agent makes when it starts, on FILE; exit 0 or 1, every problem on stderr |

`--describe` and `--effective` are not separate flags: the schema, the empty document and the
populated one are one shape, so one flag prints them. `polar-config-check`, which covered the
builder's fixed composition before the resolver used the agents themselves, is retired
(2026-10-09).

### 9.4 The document

```json
{
  "format": 1,
  "agent": { "name": "kube-consumer", "purpose": "..." },
  "requires": ["cassini", "graph-write", "encryption"],
  "sections": [
    { "name": "cassini-client", "capability": "cassini", "variables": [
      { "name": "BROKER_ADDR", "type": "text", "required": true, "secret": false,
        "default": null, "mutability": "static", "doc": "the broker's address, host:port",
        "value": null }
    ] },
    { "name": "graph", "capability": "graph-write", "variables": [
      { "name": "GRAPH_PASSWORD", "type": "text", "required": true, "secret": true,
        "value": { "env": "GRAPH_PASSWORD" }, "source": "file" }
    ] }
  ]
}
```

A secret is never a literal in the document: the value is a reference (`{ "env": NAME }`), and the
pod wires the real value through `secretKeyRef`, as today. The shutdown budget is a variable with
a minimum the code declares, so "the deployment may give more, never less" is a property of the
document and not of a rule copied into the resolver.

### 9.5 Before deploy

1. The resolver obtains the scaffold from the binary (9.7).
2. It fills each variable from, in order: the workload's own override, what a capability
   contributes (the nuon's `capabilities.nuon`: certificate directory, the Cassini address),
   the kind's default, the target's facts, then the code's default.
3. It checks: nothing required is left empty, nothing was set that the agent does not declare, no
   capability the scaffold requires is unknown to the catalog, the shutdown budget is not below the
   minimum, and `--check --config` passes, which is the agent's own startup judgement run offline.
4. It writes the populated document into a ConfigMap (`<workload>-config`, key `config.json`),
   which the agent chart mounts read-only at `/etc/polar/config.json`, and sets `POLAR_CONFIG` to
   that path.

The workload entry no longer says `needs`: the capabilities come from the scaffold, so the code
is authoritative and the catalog cannot disagree with it. What the catalog still holds is
deployment knowledge that cannot live in a binary: what a capability means for the pod (volumes,
certificates to get, services to wait for, cluster roles), the addresses of services, and the
target's facts. Those are values and resources to fill in, checked against the scaffold, not a
mirror of it. The per-workload `shutdown.budget_secs` figure disappears; the budget defaults to
the code's minimum and is overridden only to give more.

### 9.6 At runtime

`polar-env`'s `Env` is a one-method trait, `var(name)`. A `FileEnv` that serves the document's
values drops in beneath `Config`, `Section` and `config.read`, which do not change; `Source` gains
`File`, and the override layer stays above it for scheduled configuration. The environment still
carries the secrets the document refers to, and the one variable `POLAR_CONFIG`.

For the agents on the builder the document is the only source of declared variables. Agents not
yet on the builder keep reading the environment and are unaffected. Two things follow:

- `polar-healthcheck`, the liveness probe the kubelet runs in the agent's container, reads
  `POLAR_HEALTH_*` from the environment today; it reads the same document.
- Running an agent by hand is `agent --scaffold > config.json`, fill it in, and
  `POLAR_CONFIG=config.json agent`.

### 9.7 Getting the scaffold at render time

The resolver has to run the agent's own binary, so there is no copy that can go stale:

- **Locally:** `cargo run -q -p <agent> -- --scaffold`. Incremental after the first build, but a
  cold run compiles every agent.
- **For a deploy:** the built image, which is the exact artifact that will run. **Done
  2026-10-09.** `POLAR_AGENT_SOURCE=images` makes the resolver pull the image from the registry this
  machine pushes to (`POLAR_REGISTRY`, default `localhost:5000`), read its entrypoint and cmd from
  its configuration (the nix images set a cmd, not an entrypoint, so `podman run <image> --scaffold`
  would try to run `--scaffold`), and run that with the flag and no network. The same route runs
  `--check --config` on the document it rendered, with the file mounted. `up.nu` sets it, and with
  `--dev` builds and pushes the images of the self-describing agents (`resolve.nu document-images`,
  from the catalog, not from the render) in a step before the render. Without `--dev` the render
  asks the images already in the registry, and says which to push when one is missing. The source
  tree route stays the default for work on a developer's machine, and a test checks that both give
  the same record.

### 9.8 Order of work

1. `polar-env`: the document type, `FileEnv`, `Source::File`, secret references; `polar-healthcheck`
   reads it. **Done 2026-10-09** (`polar_env::Document`, `FileEnv`, `from_environment`, behind the
   `document` feature; `ProbeConfig::from_environment`).
2. The builder: the `PolarAgent` trait, the builder constructed from it, `run::<A>()` and the three
   flags. **Done 2026-10-09** (`polar_agent_builder::agent`, `AgentBuilder::into_configured`; a
   runnable example, `cargo run -p polar-agent-builder --example hello_agent -- --help`). Not yet
   used by any real agent.
3. kube-consumer end to end: declare it, scaffold it, fill it from the resolver, render the
   ConfigMap, run it on the cluster.
4. The other five builder agents; the Jira and Kubernetes variables declared and the lint
   extended. **Done 2026-10-09**, and the credentials agent's pair went further: their variables
   are declared (`settings.rs` in each) and both are catalog workloads on a document, with the
   catalog taught what their hand charts did (`config/workloads/credentials.nuon`, the
   `persistent-data`, `pinned-ca` and `escrow-exempt` capabilities, the `init` facet, and the
   entry fields `provides`, `no_start_wait` and `service`).
5. Agents that bypass the catalog, later (see `remaining-work.md`).

### 9.9 Still open

- Where an agent's long help text lives, and whether a missing one is an error in CI or a generic
  fallback (a fallback for now; the one-line purpose is required).
- Requirements that depend on configuration (a capability only when a feature is on): a static
  superset in the declaration, with the condition as a relation inside a section, as the graph's
  client-certificate trio already is.
- Versioning the document (`format`), and what an agent does with a document of a newer format.
- Whether the ConfigMap changing restarts the Job (it does not by itself; a Job's template is
  immutable, so a changed document is a changed manifest and `up.nu` restarts it as it does today).
- The Cassini broker and `polar-init` do not use the builder; the same wrapper with a declaration
  that has no capabilities could serve them, later.
