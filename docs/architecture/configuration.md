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
- A lint (`disallowed-methods` for `std::env::var` and its siblings) keeps direct reads from
  creeping back.

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

`polar-config-check --describe --json` (not yet machine-readable; today it prints for people)
returns every variable with its type, default, purpose, secrecy and mutability. That one
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
startup and the checker judges them for the environment a workload is given. Encryption and the
Kubernetes API read bring none until `polar-crypto` is on `polar-env`.

Next, in order: handing the Cassini client and the graph controller their settings from the
snapshot (today they still read the process), `--describe --json` and an `--effective` view; then the lint. After that,
the ConfigurationActor shell, then the scheduler integration, then the credentials agent's use of
it. Every step is in `remaining-work.md`.
