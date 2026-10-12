# What the scheduler carries: three kinds

Status, 2026-10-11: direction agreed with David; the configuration kind is built, and the other two have designs
(`proposals.md`, `commands.md`) that are waiting on a few decisions, listed at the end of each.

The scheduler began with three directories in the schedules repository (`permanent`, `adhoc`, `ephemeral`).
What they were meant to express is three different things, which differ in who authors them, what authority
they need, and how they behave. Reading the code: `adhoc` today is an agent type announcing its own default
schedule on `scheduler.adhoc`, and `ephemeral` is a sample of an event-triggered rule; neither does what was
intended. The directories and samples are provisional until the designs below exist.

| | Configuration | Proposal (the intent of "ad-hoc") | Command (the intent of "ephemeral") |
|---|---|---|---|
| Example | the git agent's settings and credentials | the Jira consumer: "a repo was found in issue Y" | "back up your data now" |
| Author | a person, through the repository | an agent | an admin, through a tool |
| Signed by | the author's user certificate | the proposing agent's workload certificate | the admin's user certificate |
| Meaning | desired state: the latest replaces the earlier | add to a set, with no admin step | do something once |
| Replay protection | a version that only rises | idempotent by key (the repo URL) | a unique id and an expiry |
| Kept as state | yes | yes, as a durable set with provenance | no: replaying it would run it again |
| The receiver reports | what it applied or refused | accepted, rejected or pending | success or failure |

## Principle: every scheduling event is observable in the graph

Whether it is an update from the repository, a proposal or a command, the scheduler writes it to the graph
when it receives it, and the outcome is written too. Commands most of all: an operator must be able to see
that a backup was requested, by whom, when, that it was delivered, and what became of it. Two parts:

1. **Receipt.** The scheduler processor writes a node for the item (its document as received, its target,
   its version, who signed it as far as is known without verifying) before it forwards anything.
2. **Outcome.** The receiving agent says what it did (applied, refused and why, with the signer and version)
   as an event on a topic, and the scheduler processor writes that to the graph. Agents do not each need
   graph access to be observable, and the author's tool can see a refusal. This is also how an author learns
   that a pushed document was refused, which the signed-configuration design left open.

## Configuration

What the repository holds, and the only kind it holds: desired state a person authors and signs. The signed
document, its verification and the ConfigurationActor are in `signed-configuration.md`. It targets an agent by
its name for now; instance ids come later if they are needed. A schedule's timing is not a separate field: it
becomes a typed Dynamic variable of the agent, as the older notes proposed.

## Proposal

An ordinary event from one agent to another (Cassini tells the receiver who published it), not repository
content. What is distinctive is on the receiving side. The receiver decides whether to accept, keeps the
result durably with where it came from, and can have an entry removed.

A repository URL taken from a Jira issue is attacker-controlled text. If the git agent cloned whatever it was
told, and used a stored credential for it, a crafted issue could send that credential to the attacker's host.
So the receiver applies **admission rules**, and David's direction is that these are themselves a kind of
scheduler repository content: specifications that say under what parameters a proposal of a given kind is
accepted (which hosts or organisations, which proposers, whether it needs approval), signed like any
configuration, and which agents read for the proposal kinds they care about. Credentials are bound to a host
pattern and never tried elsewhere. A proposal with no credential yet is pending until signed configuration
supplies one. "Agent X may propose kind K to agent Y" is finer than the closed capability list can say, so
this is where a Cedar policy earns its place.

## Command

Not a repository file: a file stays after it has run. It is an admin action, signed with an id that is unique
and an expiry, published by a tool. The agent keeps the ids it has run, ignores one it has seen or one that has
expired, runs it once, and reports the result. The ConfigurationActor's rising version does not fit it. The
agent declares which commands it accepts, in the same schema export that lists its Dynamic variables.

## What is built, and the order

1. **Configuration, delivery. Built.** The processor recognises a signed configuration document (an object
   with `payload` and `signature`), writes a `(:Schedule:Configuration {id})` node with the document as
   received, its target, version, source path, a `received` status and the time, and publishes a
   `ConfigurationNotification` on `agent.<name>.configuration`; every builder agent subscribes to its own
   topic and passes what arrives to the verifying `submit`. A document it cannot route is still written (as
   `unroutable`, with the reason) and goes nowhere. A version no newer than one already forwarded is written
   and not forwarded again. The unsigned schedule shape still takes the old path, untouched. The observer
   still reads only the `permanent`, `adhoc` and `ephemeral` directories, so a configuration item lives in one
   of them until the repository layout changes. Not verified on a cluster.
2. **Outcome events. Built.** The ConfigurationActor announces every document it handles on an output port; the
   infrastructure supervisor puts each on Cassini (`polar-configuration-outcome`) through whichever client it
   currently has; the processor writes a `ConfigurationOutcome` node per event and links it to the item with
   `HAD_OUTCOME`, keeping `reported_by` (the broker's identity for the publisher) apart from the agent the
   outcome names. A refusal is therefore visible, with its reason and signer. Outcomes for a proposal or a
   command will use the same path. Not verified on a cluster.
3. **Proposal design and Command design. Written 2026-10-11, awaiting David's decisions:** `proposals.md` (with `sequences/general/proposal_flow`) and `commands.md` (with `sequences/general/command_flow`). Then their code, each tests first.
4. Retire the old `adhoc` and `ephemeral` handling and samples when 3 has replaced them.
