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

# The Jira agents: configuration, connection and failure (2026-10-30, as built; fixed for 3.0.0 on 2026-11-01)

Commits cited below are the ones on the branch rebased onto main's pulled health (`e176ae6b`..`d25bbe7c`); the
review's fixes were first made on an older base, under other hashes.

jira-observer polls a Jira site for projects, groups, users and issues and publishes them to Cassini; jira-processor
writes them into the graph. This page is what an operator needs to connect a site and what the observer does when it
cannot. It was written after a real Data Center site was connected on 2026-10-30 (minikube on a managed Mac) and every
step that failed there was fixed; the fixes are in the sections below.

## What the observer reads

Four polar-env variables, section `jira` (`jira/observe/src/settings.rs`), every one Dynamic since 2026-11-01 (a signed
update changes it while the agent runs, below), the `egress` section, Static (`POLAR_EGRESS_PROXY`,
`POLAR_EGRESS_NO_PROXY`, `POLAR_EGRESS_CA_CERT`; egress.md: the cluster's setting, its CA a mounted file), and one
runtime section, `credentials` (below, "The credential"):

| Variable | | What it does |
| --- | --- | --- |
| `JIRA_URL` | required | the site's root: `https://jira.example.org` or, with a context path, `https://host/jira`; https and a host name when read with a credential (`credentials::credential_host`), as git's credentials go only over https |
| `JIRA_ANONYMOUS` | default `false` | `true` reads the site without a credential, only what it shows the public: no `Authorization` header, no `/myself` check, no `credentials` section needed, and the users observer is not started (below) |
| `JIRA_DEPLOYMENT` | optional | `cloud`, `server` or `datacenter` (any case), anything else refused (`parse_deployment`); used only where the site's `serverInfo` does not say its `deploymentType`; unset too, Cloud only for an `.atlassian.net` address |
| `QUERY_DATE` | optional | `YYYY-MM-DD` or `YYYY-MM-DD HH:MM` (or with `/`), anything else refused (`parse_query_date`); when the graph has no issue yet, the first issue pull asks only for `updated > "QUERY_DATE"`, quoted; once it has one, the first pull starts from the graph instead (below) |

`JIRA_TOKEN` and `JIRA_EMAIL` are gone since 3.0.0 (`96ddd517`): a document that still gives either is refused by name,
as any variable the agent does not declare.

Every endpoint is the site root plus `/rest/api/...`, so `normalise_jira_url` (`settings.rs`) is applied:
trailing slashes are dropped, and an address is refused, with what to set, when its scheme is not http(s), a path
segment is `rest` (a pasted `/rest/api/2`), it has a query or fragment, or it is still the placeholder
`https://example.atlassian.net` the target templates ship. The checks are in `prepare` (`agent.rs`), not the
polar-env parser, because the render's `--check` runs on targets that still hold the placeholder; the deployment and
`QUERY_DATE` are refused there too. A refusal at start is held, not ended: the agent stays up through
`polar_agent_builder::held::stay_up`, holds `Configuration` with what to set, answers health queries, and on each signed
update runs `prepare` again on the configuration in force, starting its observers once it works (below). (DECIDED, to
review: the render's `--check` does not catch a mistyped deployment or date; the parser would, but the refusal would
then not be the start-time one that pulled health reports.)

## The credential (since 3.0.0)

The token travels as the git observer's credentials do (`git/observe/src/credentials.rs`, git-controller.md): the
`credentials` runtime section of jira-observer (`jira/observe/src/credentials.rs`), one entry `{ name, host_pattern,
kind, username, secret }`, its `secret` an `EscrowedSecret` for jira-observer bound to
`jira-observer|credentials|<version>|<name>|secret` and to `host_pattern`. `polar-sign` makes it from what is typed, as
for git. The supervisor hands each version to the CryptoActor (`credentials::apply`: `RegisterSecret`, then
`ReplaceSection` keeping that name, so a version with no entry withdraws it); the CryptoActor opens it inside the custody
daemon and escrows it. On each request the observer asks the CryptoActor for the credential for JIRA_URL's host
(`CryptoClient::http_credential`, `JiraAuth::apply`): a `token` is sent as `Authorization: Bearer` (a Data Center or
Server personal access token), a `user-password` as Basic, its `username` the Jira Cloud account's email and its secret
the API token. A credential the CryptoActor will not give (none registered, not bound to the host, not text) is a
`Credentials` failure at once. jira-observer needs encryption for it (`NEEDS`), so its pod escrows a key in init.

A new token is a new signed version: it reaches the running agent (`RuntimeChanged`), and the supervisor starts the
observers again with it, which releases every hold. Without a credential, and not reading anonymously, it starts no
observer and holds `Credentials`, saying to sign the section; one arriving starts them. `credentials-agent-architecture.md`
records the CryptoActor's `HttpCredential`, the sibling of git's `GitCredential`.

## Where an operator sets it

- **URL, deployment, QUERY_DATE, anonymous reading:** `workloads.jira-observer.env` in
  `infra/targets/<target>/config.nuon` (`{ name, value }` entries; `cluster-inputs.nuon` replaces entries of the same
  name). They become literals in the agent's document, ConfigMap `jira-observer-config` (`config.json`, the settings
  under `.sections[].variables[]` as `{ name, value }`). A change reaches the pod after a render and apply (`up.nu`), or,
  without one, as a signed update to the agent (they are Dynamic). The catalog's entry gives jira-observer nothing:
  no Secret, no env. sandbox-local (u7c) reads Apache's Jira anonymously (`https://issues.apache.org/jira`, `server`,
  `QUERY_DATE` 2026-10-31; `b1789556`).
- **Token:** the `credentials` section, signed with `polar-sign` for jira-observer (above). There is no `jira-secret`;
  a `jira_token` left in a target's sops files is read by nothing.
- **Checking a connection:** `nu infra/jira-check.nu <target>` (below) asks anonymously; whether the site accepts the
  credential is in jira-observer's health.

## What it does at start, and when Jira fails

1. Nothing calls Jira until the observer has registered with Cassini (`supervisor.rs`).
2. **What the site is** (`lib.rs`, `ensure_site`, since 2026-11-01): the issue and user observers, whose requests
   differ by deployment, ask `GET /rest/api/2/serverInfo` once; its `deploymentType` (`Cloud`, or anything else for
   Server or Data Center: all four public Data Centers answer `Server`) decides, over `JIRA_DEPLOYMENT` and the
   address, which are kept when it does not say (an error status, a body that does not decode, no `deploymentType`). A
   site that cannot be reached fails the pull, and the next asks again.
3. **The token** (`lib.rs`, `check_token`; skipped reading anonymously): `GET /rest/api/2/myself`. A 401 or 403, a success naming the
   anonymous user, or the `X-AUSERNAME: anonymous` header is refused with what to check (raw vs base64, and the
   credential's kind: `token` for a Data Center personal access token, `user-password` with the email for Cloud). Data Center answers a token it does not accept as an
   anonymous request: `/myself` 401 with `x-ausername: anonymous` and no `X-Seraph-LoginReason`, `/project` 200 with
   `[]`, `serverInfo` without a version where anonymous access is locked down.
4. A token that decodes from base64 to printable text is warned about when the token is checked (`settings.rs`,
   `token_looks_base64`, on what the CryptoActor gives); a Data Center token, itself base64-looking, decodes to binary
   and does not trip it.
5. Projects, groups, users and issues each pull at once and then every 1800 s (`agent.rs:39`). **Before its first
   issue pull** the issue observer asks the state agent (`soft-state.md`) for the newest `updated_ms` of a `JiraIssue`
   in the graph, which jira-processor writes for every issue from Jira's system field `updated` (found by its `/field`
   schema, not by its display name, and passed as `polar_updated`): `Max { JiraIssue, updated_ms }`, through the state
   actor the builder gives it (`polar-agent-builder/src/state.rs`: 8 s per attempt, three attempts, a fresh
   correlation id each, `POLAR_STATE_REPLY_TIMEOUT_SECS` and `POLAR_STATE_ATTEMPTS`; an `Unavailable`, such as no schema
   yet, it asks again itself every 10 s for up to 2 minutes before the observer hears it). With a time `t`, the first pull asks `updated >= -Nm`, N the minutes from `t` to now
   plus the 300 s overlap, rounded up, at least 1 (`issues.rs`, `cursor_minutes`); relative, so the Jira user's time
   zone does not enter. With no issue in the graph it is full, or from `QUERY_DATE`. A refusal, a graph the state
   agent cannot read, or no answer is retried on the backoff below, and nothing is pulled until it says: never a full
   pull (`issues.rs`, `start_from`). Later pulls ask for `updated >=` the start of the last SUCCESSFUL pull plus 300 s
   overlap, never less than 35 minutes (`incremental_window`), so a long outage leaves no gap. Every issue pull is
   ordered `ORDER BY updated ASC`, on Data Center and Cloud alike (`issue_jql`; a full pull on Cloud is bounded by
   `project is not EMPTY`), so a pull cut short leaves the graph's newest update meaning everything older was
   written. The projects, groups and users observers ask nothing.
6. **No Jira failure is fatal** (`lib.rs`, `settle`): an HTTP error, a connection, DNS or TLS failure, a timeout
   (connect 15 s, request 120 s, `supervisor.rs`), a decode error or a refused token is logged (the host kept out of
   messages above debug). One that may pass is retried after 30 s, doubling to 15 minutes (`lib.rs`, `RETRY`); after a
   success the next pull is the normal interval and the token is checked again after any failure. Each observer keeps
   its own retry state. One a retry cannot fix is held (below). A configuration refusal at start (the URL, deployment
   and date checks) is held too, not ended (above). Since 2026-11-01:
   - **By kind** (`FailureKind`): `Transient` (unreachable, timed out, 5xx, 429, the state agent not saying),
     `Credentials` (401 or 403 once the token has been checked again in the same pull, a refused token, or any 401/403
     reading anonymously) and `Configuration` (404, 410, 400, or a body that does not decode from `/myself`,
     `/serverInfo` or `/field`). A 401/403 before the recheck is `Transient`: the next pull checks the token first. The
     observer keeps its last failure (`JiraObserverState::failure`) until a pull succeeds and logs `Credentials` and
     `Configuration` at error, with what to check. Only `Transient` is retried on the backoff (`8c2ec420`, below).
   - **`Retry-After`** on a 429 or a 503 (seconds or an HTTP date) makes the wait at least that long, never longer than
     the base interval (`retry_after`).
   - **A walk resumes where it failed**: each observer's walk is kept in its state while a page fails (`IssueWalk`,
     `UserWalk`, `GroupWalk`), and the next attempt goes on from that page; pages already read are neither asked nor
     published again. An issue walk asks what its first attempt asked: its relative window (`updated >= -Nm`) reaches
     back by the time since the walk began (`Filter::jql`), so its oldest end never moves later while it goes on (Jira
     measures `-Nm` from each request; an end that moved later would drop issues from the front of a walk ordered
     oldest first and shift the rest under the offset). The next incremental pull reaches back to when the completed
     walk began. On Cloud a failed walk goes on from its first page, still asking what it asked (its page tokens are
     not known to outlive a failure).
   - **Held through the agent's health** (`8c2ec420`): each observer is given the agent's `HealthHandle` (the
     supervisor's `resources.healthcheck`, through `observer_args`, in `JiraObserverArgs`). In `settle` a
     `Credentials` or `Configuration` failure calls `hold` with one line, `<observer>: <what failed>; <what to set>`
     (`JiraError::brief`: the request's path without its query, at most 80 characters; the whole fits a report's 200
     characters and never carries the token or the site's address), and the observer schedules no pull: the agent stays
     up and reports it. What tries again is a signed update (a new credential, new settings: the supervisor starts the
     observers again, releasing every hold) or the hourly rejuvenation. A success releases the hold its own observer
     placed. Several observers may hold at once (DECIDED, to review): each releases only its own, so one observer's
     success never ends another's hold; the health actor keeps one standing problem, the latest, and each is logged at
     error by its observer.
   - **Settings change at run time** (`e0901170`): the supervisor subscribes to the ConfigurationActor's `changes()`;
     an update naming a `jira` (or egress) variable makes it run `prepare` again on `current()`. Settings that work
     replace the ones in force and the observers start again; settings that cannot work stop them, and the supervisor
     holds `Configuration` with why until an update mends them. The scheduler replays the signed documents to an agent
     that has just registered, so after each restart the render's env applies first and the signed update just after.
   - **Held at start, then mended** (`8635c729`): `held::stay_up` takes a `TryAgain`; jira-observer's runs `prepare` on
     the configuration in force and, if it works, starts the observer supervisor on the infrastructure the held agent
     built (`Infrastructure::Built`).
   - **Reading anonymously, the users observer is not started** (`05851c1a`, DECIDED, to review): a Data Center lists
     users only to a login (Apache answers 401 to `/rest/api/2/user/search` and 404 to the picker), so it could only
     hold `Credentials`. The start log says so. Groups stay: anonymously the picker answers 200 with no groups.

Before 2026-10-30 any Jira error ended the process: the pod's `RestartAllContainers` rule (deployments.md) restarted
the whole pod, the restart removed the container and its log, and the next start waited up to five minutes in
polar-init for the credentials agent's renewal rule (`TooSoon`, credentials-agent/common/src/issuance.rs). That is why
the failure on the Mac was invisible for a day. The last log lines of a failed container are now its termination
message (deployments.md, "termination message"), but the kubelet's removal record replaces it within about 2 s, so it
is seen only by someone watching the pod; a cause that outlives the container needs a file on a volume that survives
the restart (open, below).

## Checking a connection: `nu infra/jira-check.nu <target>`

Runs, from a short-lived pod on jira-observer's node pool, the requests the observer makes, with its document, and
prints only facts safe to share: the URL's shape, whether the agent reads anonymously, the class of address it
connected to (`100.64/10` as Zscaler Private Access gives, private, public), the TLS verify result, and the HTTP status
of `serverInfo`, `myself` (with whether it answered as anonymous) and `project` (how many it shows the public), then
what each pattern means. It never prints the host or URL (INFRA.md, `infra/tests/jira-check.nu`). Since 3.0.0 it asks
anonymously: the credential is the agent's signed section, which only its CryptoActor opens, so no other pod can
present it. Whether the site accepts it is in jira-observer's health (a refused token held as `Credentials`).

## Networks: Zscaler, VPNs, private CAs and proxies

- A Mac behind Zscaler reaches internal sites through Private Access: the Zscaler client answers DNS for them with an
  address in `100.64.0.0/10` and carries the connection. Pods resolve through CoreDNS and the minikube VM, which may get
  the public answer instead and take another route (often a front end that drops the `Authorization` header and passes
  the request on as anonymous). `jira-check.nu`'s address class shows which. On 2026-10-30 it was not the cause (the
  token was), so the fix (a CoreDNS forward for the site's domain to the host) is written down, not built.
- An internal CA, or Zscaler Internet Security's TLS inspection, needs the CA trusted, and a proxy needs the proxy.
  BUILT (egress.md): an optional `cluster.egress` setting (`proxy`, `no_proxy`, `ca_file`) becomes the declared
  `egress` section (`POLAR_EGRESS_PROXY`, `POLAR_EGRESS_NO_PROXY`, `POLAR_EGRESS_CA_CERT`, `polar::egress`) of the agents
  that call outside services, the CA added beside the image's roots, in-cluster names never proxied; a target that sets
  nothing renders the same pods. `PROXY_CA_CERT` is gone.

## Open

- The user picker: FIXED 2026-11-01 (`55461610`, below, 4): Server and Data Center are read by the user search.
- The cursor: BUILT 2026-10-30, and rebuilt the same day on the review (`soft-state.md`): the state actor asks, and the
  time is the system field `updated`, so a Jira in another language works too. Before, whether the full pull was done was held
  in memory, so every restart (about every half hour, with rejuvenation) pulled everything again or from the fixed
  `QUERY_DATE`, and a user moved `QUERY_DATE` by script as a hand-made cursor. Now the first pull after a start begins
  at the graph's newest issue; `QUERY_DATE` applies only to a graph with no issue. Seen on a cluster, the first build
  and the rebuild (soft-state.md, "Seen on a cluster" and "The rebuild, seen on a cluster"). Kept open: an issue updated
  during a walk by `startAt` moves to its end, so the issues after it shift back by one and one of them is skipped,
  as before (the window no longer drifts later under the walk since `47c85eca`); and a graph whose issues were all
  written before jira-processor wrote `updated_ms` answers that none has it, so the first start after the upgrade
  pulls everything (or from `QUERY_DATE`) once, as before.
- A failed container's cause outlives it only if written to a volume that survives the in-place restart.
- FIXED 2026-11-01 (`d5bb9200`): the processor made `CHILD_OF` from the `parent` field's `name`
  (`process/src/issues.rs`, the `parent` branch), but Jira's `parent` carries `id`, `key`, `self` and `fields`, no
  `name`, so a sub-task's `CHILD_OF` was never written; it is made from the `key` now. Apache's `/field` does not list
  `parent`, so it reaches the processor under that name. Open: a site whose `/field` lists `parent` would have it renamed
  to its display name, and the processor would not find it (it could travel as a system field, `polar_system`).
- FIXED 2026-11-01 (`05851c1a`): the users observer could read nothing anonymously on Apache: its user search answers
  401 to an anonymous request (and its user picker 404, "The user named '{0}' does not exist", for any query, so the
  review's 404 for `query=''` was the anonymous refusal, not the query). Read anonymously it is not started. The group
  picker answers no groups anonymously, not a refusal, so the groups observer stays.

## The review before 3.0.0 (2026-11-01)

A read-only review of both agents, ranked, with two claims checked against a real Data Center (`issues.apache.org/jira`,
anonymous, one request each). Paths under `src/agents/jira/`. Fixed 2026-11-01 for 3.0.0 as each item says, against a
Data Center (the site is on-prem); the Cloud-only parts (ADF rich text, `accountId`-only users) are out of scope, and
nothing was made worse for Cloud. Verified with tests (wiremock sites paging as a Data Center does, and real issues
from Apache's Jira, `jira/common/fixtures/apache.rs`) and mutation checks; not yet run on a cluster.

1. **A start-time configuration refusal ends the process and the pod loops** (`observe/src/agent.rs:85`; the decided
   split, remaining-work.md "HEALTH IN THE GRAPH, PULLED", is unbuilt for start). FIXED: pulled health holds it at start
   (`d25bbe7c`, `held::stay_up`), and a signed update that mends it starts the observers (`8635c729`); the run-time
   failures a retry cannot fix are held too (`8c2ec420`). This workstream made the refusals precise (7, below) and kept
   the observers' failures by kind (5).
2. **Changelog authors without an email drop the whole issue.** `JiraAuthor` requires `emailAddress`
   (`common/src/types.rs:79-91`); CHECKED: Apache's changelog authors carry `name`, `key`, `displayName`, `avatarUrls`,
   `active`, `timeZone`, `self` and no email, as any site that hides email does. `handle_issue`
   (`process/src/issues.rs:458-463`) logs a warning without the key and returns `Ok`: nothing written, nothing unhealthy.
   On Cloud also: users have only `accountId` (`JiraUser`, `JiraAuthor` require `name` and `key`), and
   `/rest/api/3/search/jql` returns rich text as ADF, which `FieldValue`'s five fixed tiers (`types.rs:118-188`) cannot
   hold, so a non-empty description fails to decode. FIXED (`4a458303`): every identity field of `JiraAuthor` and
   `JiraUser` is optional (an author is recorded by `key`, else `name`, else `accountId`; a user is keyed by `key`, else
   `name`, and one with neither is skipped, logged); an issue without `changelog` decodes; a payload that does not
   decode is logged at warn with its key (`c7138c6a`). ADF stays out of scope.
3. **A field name the graph refuses loses the issue for good.** Field names are sanitized, not validated
   (`process/src/issues.rs:184-199`); `validate_cypher_identifier` (`lib/src/graph/controller.rs:133-150`) refuses `.`,
   `:`, `'`, `&` and non-ASCII, and the issue's upsert is op 0 of one transaction, so one bad name (`Est. hours`,
   `Fälligkeitsdatum`) fails it, unretried. The cursor then passes over it, since newer issues are written. FIXED
   (`c7138c6a`): `keep_what_the_graph_accepts` takes out each property of each node and edge whose name the graph
   refuses, logged at warn with the issue's key; the rest is written. Apache's own field list has one such name,
   `Issue & fix info`.
4. **Pagination:** the group picker takes no `startAt` (`observe/src/groups.rs:66-94`): it re-fetches the first page
   and never sees later groups; the user picker's `query=''`: CHECKED, Apache answers 404, so the users observer backs
   off forever on that Data Center; a missing `total` (DC) or `isLast` (Cloud) ends a walk after one page, unlogged.
   The deployment guess (`observe/src/lib.rs:215-229`) can be replaced: `serverInfo` carries `deploymentType` (CHECKED:
   `Server` on all four public Data Centers); `/rest/api/2/search/jql` does not exist on Data Center (CHECKED: 404).
   FIXED (`55461610`): the group picker is paged as a Data Center pages it, by `exclude` (each request leaves out the
   groups read), to its total or a page with nothing new, bounded at a 6 KiB request (logged); Server and Data Center
   users are read from `/rest/api/2/user/search?username=.` (every user, active and inactive), paged by `startAt` to an
   empty page, Cloud keeps the picker; a missing `total` is logged and the walk goes on to an empty page, a missing
   `isLast` is logged and the page token decides. CHECKED 2026-11-01: Apache's user picker answers 404 to an anonymous
   request for any query (`query=a` too), and its user search 401: the 404 was the anonymous refusal. The deployment
   comes from `serverInfo` (`c9c40617`, 7).
5. **Every non-2xx is one retry** (`observe/src/lib.rs:362-378`, `settle` :436): 404, 410, a refused token, a malformed
   body back off forever and never reach health; 429's `Retry-After` is ignored; a failure on page N restarts at page 0.
   FIXED (`47c85eca`): failures are classified (`FailureKind`: `Transient`, `Credentials`, `Configuration`) and kept by
   each observer until a success, `Retry-After` is honoured on 429 and 503, and a walk resumes at the page that failed
   (above, "What it does at start", 6). The kinds a retry cannot fix are held through the agent's health since
   `8c2ec420`, and not retried.
6. **The processor's shutdown drops queued writes:** `stop_and_wait` (`process/src/supervisor.rs:286-305`) is served
   before ordinary messages, so ops still in the graph controller's mailbox are lost at each rejuvenation;
   `drain_and_wait` keeps them. Writes while Neo4j is down are dropped, not retried. MERGE keeps every write repeatable,
   but never removes an old `ASSIGNED` edge or a cleared field's property. FIXED (`9e63fe0f`): the shutdown drains
   the graph controller (`drain_and_wait`, `stop_graph_controller`), so every op already queued is written before it
   stops. Not changed: writes while Neo4j is down are still dropped, and MERGE still never removes an old edge or
   property.
7. **Start validation lets through:** a mistyped `JIRA_DEPLOYMENT` (falls back to the guess), `QUERY_DATE` unchecked and
   unquoted in the JQL (a time gives a 400, retried forever), the shipped placeholder `JIRA_EMAIL`
   `jira-user@example.org` (a Data Center then gets Basic auth, refused forever). FIXED (`c9c40617`): each is refused
   at start with what to set (`JIRA_EMAIL` is gone since `96ddd517`, the credential's `username` now); `QUERY_DATE` is said back in one form and quoted in the JQL; the deployment type is
   `serverInfo`'s `deploymentType`, the setting and the address only where it does not say.
8. **English display names:** the processor finds assignee, reporter, creator, type and project by display name
   (`process/src/issues.rs:236-278`), so on a site in another language those edges are not written (the cursor is
   language-safe; the "another language works too" above is about the cursor only). FIXED (`184f811b`): the observer
   moves those five system fields out of `fields` to beside them, as `polar_system`, by system id (`/field`'s
   `schema.system`, or the field id that is the system name), as it passes `updated`; the processor makes ASSIGNED,
   REPORTED, CREATED, HAS_TYPE and PART_OF from there only (`system_field_ops`). Found on Apache: a custom field named
   "Project" (`customfield_12312823`) took the system field's place in the renaming whenever it came first, so the
   display name was not safe in English either.
9. **Tests:** no multi-page walk, no 429, no realistic issue JSON through `JiraIssue`, no graph write, nothing end to
   end; the fake site in `observe/tests/observers.rs` serves issues without `changelog`, and the u7c fake is not
   committed. FIXED (`53f160f7` and each fix's own tests): real issues from Apache's Jira (`jira/common/fixtures/apache.rs`,
   its comment says how they were read and trimmed) through the observer's preparation, pinned, and through
   `JiraIssue` and the processor's ops, every op compiled as the graph controller does; multi-page walks of issues,
   users and groups; 429 and 503 with `Retry-After`; resume at the failed page; a refused field name; anonymous reading.
   Property tests: a `QUERY_DATE` round trip, a refused name never failing an issue, system fields matched by id
   whatever their display names, a resumed walk never reaching back less than where it began. Still not committed:
   the u7c fake; still nothing end to end against a graph.

The stale lines this section listed are corrected above: egress is built (`polar::egress`; `PROXY_CA_CERT` is gone,
the section is `POLAR_EGRESS_*`), and the cursor was seen on a cluster.

Anonymous reading (`d57d7929`): `JIRA_ANONYMOUS=true`, an explicit opt-in (the table above). It sends no
`Authorization` header and makes no token check, and the start log says the observer reads only what the site shows
the public. For the run against Apache's Jira. It was NOT YET WIRED in infra while the catalog injected `JIRA_TOKEN`
from a Secret (refused beside the opt-in); since the credential is a signed section the catalog injects nothing, so a
target only sets `JIRA_ANONYMOUS` (`73a29552` tests it; u7c does, `b1789556`).

### Decided while fixing (2026-11-01), to review

Decided where the review and the design principles were silent, by the agent that built the fixes:

1. **A 400 is `Configuration`** beside 404 and 410: the same request cannot succeed (a JQL Jira refuses).
2. **"A configuration endpoint" is one that describes the site:** `/myself`, `/serverInfo`, `/field`. A body that does
   not decode from a search or a picker is `Transient`.
3. **`Retry-After` is capped at the base interval** (1800 s): a site asking for longer is asked again at the next
   ordinary pull, and a hostile header cannot pause the observer for days. Seconds and HTTP dates are both read
   (chrono, added to jira-observer).
4. **An issue walk re-anchors its relative window per page** (by the time since it began), rather than freezing the
   JQL text, so a resumed walk (after up to 15 minutes of backoff) skips nothing at its oldest end. On Cloud a failed
   walk restarts at its first page instead of reusing its page token.
5. **The group picker is paged by `exclude`**, bounded at a 6 KiB request; past that the rest of the groups are not
   read, logged. A Data Center offers no other paged list of groups.
6. **Server and Data Center users come from `/rest/api/2/user/search?username=.`**, not the picker, which cannot page;
   Cloud keeps the picker as it was.
7. **The deployment refusals and the date check are at start, not in the polar-env parser** (above, "What the
   observer reads"), so the render's `--check` does not catch them.
8. **`serverInfo` is asked once per observer that needs it** (issues, users); a site that answers without a
   `deploymentType` leaves the setting, one that cannot be reached fails the pull.
9. **The system fields leave `fields`** (`polar_system`), so an edge-making field is never also a dynamic node, and a
   display name is never read for one.
10. **`JIRA_EMAIL` beside `JIRA_ANONYMOUS` is refused** too (a section relation), as half of Cloud's credential.
    SUPERSEDED by `96ddd517`: `JIRA_EMAIL` is gone, the credential's `username`.
11. **An issue without `changelog`, and a user without a key, decode**; the user is keyed by its name, or skipped.

### Decided after the review (2026-11-01), to review

Decided by the agent that built the health hook-up, the credential and the run-time settings, where the maintainer's
answers and the design principles were silent:

1. **Each observer releases only the hold it placed**; the health actor keeps one standing problem, the latest, and
   every hold is logged at error by its observer (`8c2ec420`; the maintainer has these for review).
2. **A held observer schedules no pull.** What tries again is a signed update (the supervisor starts the observers
   again, releasing every hold first) or the rejuvenation.
3. **The hold's message is `<observer>: <what failed>; <what to set>`**, what failed cut to 80 characters, so what to
   set always fits the report's 200.
4. **`HttpCredential` is a sibling of `GitCredential`**, not a generalisation: git is given a username for a token
   (`oauth2`), which an HTTP API is not (credentials-agent-architecture.md). Like git's it hands the caller what the
   request needs rather than an HTTP I/O actor sending it.
5. **`JIRA_EMAIL` is the credential's `username`**, by git's `user-password` precedent: a Cloud API token is a
   `user-password` with the account's email.
6. **The section takes one entry**: the observer reads one site.
7. **A credential goes only over https, to a host name** (git's rule): a site read with one must be https; reading
   anonymously, http is still read. The u7c fake Jira, if it is to be read with a credential, must serve https (the
   egress CA can trust its certificate).
8. **Without a credential the supervisor holds `Credentials` at once**, which also shows at each start until the
   scheduler's replay brings the section.
9. **Starting again is stop and respawn** of the observers, for a new credential or new settings, never a change made
   inside a running observer.
10. **Reading anonymously, the users observer is not started** (above), on Cloud too.
11. **jira-check.nu asks anonymously**: no pod but the agent's can present the credential.
12. **No `unset` entry for targets**: proposed for anonymous targets while the catalog injected the token; not needed
    once it injects nothing.
13. **The old hashes in this page** were the review fixes' before the rebase; replaced by the rebased ones.


## Seen on a cluster (u7c, sandbox-local, 2026-11-01)

Main at `8e2967b1`, every image rebuilt and redeployed; jira-observer reading `https://issues.apache.org/jira`
anonymously (`JIRA_ANONYMOUS` true, `JIRA_DEPLOYMENT` server). The graph still held the fake Jira's 120 issues from
2026-10-30, so the first pull started at the fake's newest (`2026-10-31 13:13:41Z`), not at `QUERY_DATE`.

- **Start:** anonymous, the users observer not started, `serverInfo` read (`deploymentType "Server"`), no restart loop.
- **Completeness:** every issue Apache answered as updated since the cursor's start was in the graph with its
  `updated_ms`: 656 of 656 after the first pull, 668 of 668 after the rejuvenation, none missing, none with an older
  update time than Apache's (key sets compared, one search walk of 100 a page). 30 further `JiraIssue` nodes hold only
  `key`: the parents of sub-tasks, made by the `CHILD_OF` fix (none was ever written before).
- **Across a rejuvenation (22:30:38, in place):** the new process's first pull started at the graph's newest issue
  (22:04:34, written by the 22:04 pull); polar-init did not wait on the renewal rule.
- **Pulled health:** 14 `(:AgentHealth)`, every agent `healthy`.
- **A signed update that cannot work, then one that can** (`polar-sign sign --push`, settings version 1 then 2):
  `JIRA_URL` set to the placeholder was applied the same second (`changed=["JIRA_URL"]`), the observers stopped, the
  agent held `Configuration` and stayed up, the next round wrote `unhealthy` / `configuration` with its transition;
  version 2 (Apache's address) released the hold ("the standing problem (configuration) is resolved"), respawned the
  observers, the pull resumed from the graph's newest issue, and the next round wrote `healthy`. No container restart
  from either (the container's restart count stayed at the rejuvenation's 1).

Found, not fixed:
- **Projects never reach the graph.** Apache lists 674 projects (all decode as `JiraProject`); the graph has 51, the
  ones issues name (`project_ops`). The listing after the respawn (22:41:42) went to the processor as one `UpsertNodes`
  of 674 items and failed 30 s later with `ConnectionTimedOut`. The earlier pulls (21:34, 22:04, 22:30) left neither an
  error nor the nodes; not yet explained (a batch published before a restarted processor subscribes is lost by design,
  delivery being best attempt, but 22:04 had both up). To find, test first: a large `UpsertNodes` against a real Neo4j.
- **An empty batch is an empty statement:** `UpsertNodes { items: [] }` (groups read anonymously: none) compiles to
  `""`, which Neo4j refuses (`SyntaxError: Unexpected end of input`), counted as a graph op failure.
- **A restart between two health rounds leaves no transition:** `uptime_secs` falls, `transitions` does not show it.
- **The hold's message from a signed update** says "set workloads.jira-observer.env in the target's config.nuon", the
  render-time remedy; for a setting a signed update made, the remedy is another signed version.
- **polar-sign:** `--push` needs the git server's name resolved to the forward (`http.<url>.curloptResolve`, as
  `just polar-git-config` prints) and its cached clone (`~/.cache/polar-sign/schedules`) cannot fast-forward after a
  cluster's schedules repository is seeded anew; `--workdir` with a fresh folder worked.
- **Seen on the way:** `~/.config/polar-admin/djshepard.chain.pem`'s first certificate is not the one
  `djshepard.key` belongs to (`djshepard.crt` is); the up.nu Neo4j forward on 7473 was tied to a replaced pod (not
  probed, by design).
