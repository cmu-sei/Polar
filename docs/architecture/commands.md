# Commands

Status, 2026-10-11: design, decided (see the end); the build has begun with the shared `type` in signed payloads, the capability and the command verifier. Direction from David: a command is a one-time instruction from an
administrator to an agent, "make a backup of your data now", and above all it must be observable: that it was
requested, by whom, that it was delivered, and what became of it. This is the third of the three kinds in
`scheduling-kinds.md`; the sequence is `sequences/general/command_flow.mmd`.

## What it is, and how it differs

| | Configuration | Command |
|---|---|---|
| Meaning | the state an agent should be in | something to do, once |
| Replaying it | harmless: the same state again | harmful: it would run again |
| What orders it | a version that only rises | a unique id and an expiry |
| Kept as state | yes | no: only its record is kept |
| The receiver reports | what it applied | each step: accepted, started, finished or failed |

The existing `ephemeral` handling is something else again (an event pattern that starts an agent), as
`scheduling-kinds.md` records, and is not what this replaces.

## The command document

Signed exactly like a configuration document (`signed-configuration.md`: the same two fields, the same
`polar-signing`, the same chain to the pinned CA), with a payload of its own:

```
{ type: "command"
, id:         unique, never reused (a UUID)
, target:     { agent: "git-agent" }
, command:    "backup-now"
, args:       { ... }                 only what the agent declared, only text and numbers
, issued_at:  seconds since the epoch
, not_before: optional
, not_after:  required: after this it is never run
}
```

`not_after` is required, and a verifier refuses one more than a day (by default) after `issued_at`, so a
signed command cannot wait indefinitely for a moment to be used. Everything that matters is in the payload,
so it is all signed: which agent, which command, with what arguments, and for how long.

## Authority

Version one needs a capability for it. The capability set is a closed list in `cert-issuer-common`
(`ProvisionAccount`, `Grant`, `Revoke`, `UpdatePolicy`), and none of them means "may run commands", so a new
member, `c:run-command`, is added, issued to an administrator the way the others are
(`polar-admin-grant provision-account --capability c:run-command`). It is coarse: it permits any command to any
agent. The finer rule, "may run `backup-now` on `git-agent` but not `rotate-keys`", is a policy question for
Cedar (`polar-policy`) behind the same authority trait the configuration verifier uses.

## What an agent declares

A command is not free text. An agent says which it accepts, in the same schema export (`<agent> --scaffold`)
that lists its variables, in a `commands` section: its name, a description, its arguments with their types, how
many may run at once, how long it may take, and its **delivery semantics**:

- `once`: it must never run twice. It is run only if the ledger says it has not been dispatched before.
- `idempotent`: running it again is safe, so a redelivery is acceptable.

The same schema drives the typed template, so an administrator builds a command in Dhall and a misspelt
argument or a command the agent does not have fails before it is signed. A command the agent does not declare
is refused. Arguments are checked all or nothing, as configuration values are, and a command may not take a
secret argument.

Domain code registers a handler for each declared command with the builder (a `with_command` beside
`with_subscription`) and writes only the handler. Everything below is the builder's.

## What the receiver does

The verification is the configuration verifier's, up to the point where they differ. In order, refusing at the
first failure, and **announcing each outcome**:

1. The document has the layout, the chain verifies, the signature covers the payload, and `type` is
   `command`.
2. It is addressed to this agent.
3. The signer holds `c:run-command` (Cedar later).
4. It is within its time: `not_before` has passed and `not_after` has not (with the clock skew the cluster
   tolerates, stated, not assumed).
5. The command is declared, and its arguments parse.
6. The id has not been seen by this agent (a small set kept for the life of the process).
7. It is not exclusive with one already running.
8. The agent is not draining. A command is not started while the agent is shutting down.

Then the handler runs under the declared timeout, and the receiver announces `started` and, when it ends,
`succeeded` or `failed` with a reason and how long it took. A command that is refused at any step is announced
as refused, with the step. No argument value is ever in an announcement: only the command name, the id and the
reasons.

## At most once, and what "unanswered" means

An agent that restarts loses its set of seen ids, so the agent alone cannot promise that a command does not
run twice. The **scheduler is the ledger**: it records a command when it receives it, and marks it
**dispatched** before it publishes it to the agent, and it never dispatches an id that already has a dispatched
mark. So a `once` command is delivered at most once, and an agent restart cannot cause a rerun. The expiry
bounds every other case.

The converse is also visible. A command that was dispatched and has no outcome by its `not_after` is recorded
by the scheduler as **unanswered**: the agent was down, or ran it and died before saying so. There is no
automatic retry. If it matters, an administrator issues a new command, which has a new id. Whether a
`once` command that was dispatched and unanswered actually ran is a question the record cannot answer, and the
record says so rather than guessing.

## Observability

Every step is in the graph, which is the principle in `scheduling-kinds.md`. The scheduler writes a
`(:Schedule:Command {id})` node when it receives the command, with the document as received, its target, the
command name, the expiry, the signer if known without verifying, and a status. Outcomes are written as
`(:Schedule:CommandOutcome)` nodes linked `HAD_OUTCOME`, one per event, so the trail is kept: `dispatched`,
`refused`, `accepted`, `started`, `succeeded`, `failed`, `duplicate`, `expired`, `unanswered`. Each carries the
agent's reason and the broker's identity of whoever reported it (`reported_by`), kept apart from the agent the
outcome says it is about, as for configuration outcomes. The outcomes travel on the same path as
`ConfigurationOutcome`.

## How a command reaches the scheduler

This is open, and it decides how the administrator's tool works. A person's tool is not a Cassini client.

1. **A front door on the scheduler:** an mTLS HTTP endpoint that takes a signed command document, authenticates
   the sender by their user certificate (to keep strangers out, not to trust the content: the receiver verifies
   the document itself), writes it to the graph, and dispatches it. Immediate; no history left in a repository;
   a new service surface to build and run, like the git server or the credentials agent's front.
2. **The repository:** the tool writes the signed document into a `commands/` directory and pushes. Everything
   already exists (the git server, the observer, signature verification, the ledger, the graph). Delivery
   follows the push within seconds. The cost is that a command file stays in the repository after it has run,
   which the ledger and the expiry make harmless but which is clutter and a permanent record in git, and that
   an action with an expiry is committed through a mechanism built for state.

Recommendation: build version one on the repository, because it needs nothing new and the ledger is what makes
it safe, and add the front door when immediacy or tidiness matters. That differs from what we first said (not a
repository file), and the reason is that the ledger and the expiry remove the objection that made a file
dangerous.

## Safety

- **Rejuvenation and draining.** Agents are restarted on a schedule and on a failed health check. A command
  that is running when the agent is told to drain is allowed to finish within its timeout or reported as
  `failed: interrupted`, never silently lost. A command with a long timeout should say so in its
  declaration, so a deployment's shutdown budget covers it (the builder already refuses a budget smaller than
  the code needs).
- **Clock.** Expiry depends on the agent's clock. Skew is a stated tolerance, not an assumption.
- **Blast radius.** One target per command in version one. Nothing here addresses "every instance".
- **Two people.** A dangerous command might need two signers. The document has one signature block today, so
  this is a later change, noted so it is not forgotten.

## What changes in the code

- `cert-issuer-common`: the `RunCommand` capability, and `polar-admin-grant` can provision it.
- `polar-signing` and `signed.rs`: the document `type`, and a command verifier beside the configuration one.
- Builder: command registration, the checks above, the seen-set, the announcements.
- Scheduler processor: receive, ledger, dispatch with the mark, the unanswered sweep, and the graph writes.
- `--scaffold` and the typed templates: the `commands` section.
- `polar-sign`: build and sign a command from the template.

## Built so far (2026-10-11)

The verifier, `CommandVerifier` in `polar-agent-builder/src/command.rs`, which `UpdateVerifier::commands()` gives
with the same trust, clock, revocation and agent and its own authority (`c:run-command`). It runs checks 1 to 4
above (layout and signature, `type` is `command`, the payload read strictly, addressed to this agent), then the
authority, the revocation hook, and the time. The time rules are a pure function, `check_window`: a window that
contradicts itself (expires before it is issued, starts before it is issued, starts at or after it expires) or is
longer than a ceiling (a day by default) is refused before the clock is looked at; then, **the start is lenient
by the skew** (a clock a little behind does not refuse a command that is due, and a command issued in the future
beyond the skew is refused) and **expiry is strict** (at `not_after` it may run, a second later it may not,
whatever the skew). The payload is read strictly: exactly its fields, an `id` and a name of a sound shape,
arguments of text or whole numbers only (no boolean, float, null or nesting), at most 32 of at most 4096 bytes,
and `not_after` required. A float cannot be in a signed payload at all: the signer refuses it.

Not built: the declaration of commands in the schema export, the agent's own checks (declared, arguments, seen,
exclusive, draining), the handler registration, the outcome announcements, the scheduler's ledger and the
unanswered sweep, the repository transport, and `polar-sign`.

## Build order, tests first

1. The command verifier and the time rules as pure functions: a window, skew at both ends, a missing
   `not_after`, an over-long validity, a wrong type, a wrong target, an unauthorised signer, an altered
   argument.
2. The agent-side checks and the announcements (every step), with a fake handler: runs once, a duplicate, a
   refused argument, a draining agent, a timeout, a handler that fails.
3. The processor's ledger: dispatched before published, never dispatched twice, an unanswered command at expiry.
4. The transport, then `polar-sign`.

## Decisions (David, 2026-10-11: all four confirmed, repository transport first)

1. **The capability.** A new `c:run-command`, coarse in version one with Cedar later. Confirm.
2. **The transport.** Repository first, front door later (recommended), or the front door first.
3. **At most once is the scheduler's ledger**, with the agent's own seen-set as a second line. Confirm.
4. **A `type` in every signed payload**, as in `proposals.md`.
