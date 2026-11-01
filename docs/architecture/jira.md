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

jira-observer polls a Jira site for projects, groups, users and issues and publishes them to Cassini; jira-processor
writes them into the graph. This page is what an operator needs to connect a site and what the observer does when it
cannot. It was written after a real Data Center site was connected on 2026-10-30 (minikube on a managed Mac) and every
step that failed there was fixed; the fixes are in the sections below.

## What the observer reads

Six polar-env variables, section `jira` (`jira/observe/src/settings.rs`), all static (the ConfigurationActor refuses
changes to them, signed-configuration.md), and the `egress` section (`POLAR_EGRESS_PROXY`, `POLAR_EGRESS_NO_PROXY`,
`POLAR_EGRESS_CA_CERT`; egress.md):

| Variable | | What it does |
| --- | --- | --- |
| `JIRA_URL` | required | the site's root: `https://jira.example.org` or, with a context path, `https://host/jira` |
| `JIRA_TOKEN` | secret; required unless `JIRA_ANONYMOUS` | the token, from Secret `polar/jira-secret`, key `token`; unset without `JIRA_ANONYMOUS` is a configuration error, set with it is refused as contradictory (`TokenUnlessAnonymous`, judged with every other problem) |
| `JIRA_ANONYMOUS` | default `false` | `true` reads the site without a credential, only what it shows the public: no `Authorization` header, no `/myself` check; `JIRA_TOKEN` and `JIRA_EMAIL` are then refused |
| `JIRA_EMAIL` | optional | set: Basic `email:token` (a Jira Cloud API token); unset: `Bearer <token>` (a Data Center or Server personal access token) (`lib.rs`, `JiraAuth::from_parts`). The templates' placeholder `jira-user@example.org` is refused at start (`refuse_placeholder_email`) |
| `JIRA_DEPLOYMENT` | optional | `cloud`, `server` or `datacenter` (any case), anything else refused at start (`parse_deployment`); used only where the site's `serverInfo` does not say its `deploymentType`; unset too, Cloud only for an `.atlassian.net` address |
| `QUERY_DATE` | optional | `YYYY-MM-DD` or `YYYY-MM-DD HH:MM` (or with `/`), anything else refused at start (`parse_query_date`); when the graph has no issue yet, the first issue pull asks only for `updated > "QUERY_DATE"`, quoted; once it has one, the first pull starts from the graph instead (below) |

Every endpoint is the site root plus `/rest/api/...`, so `normalise_jira_url` (`settings.rs:115`) is applied at start:
trailing slashes are dropped, and an address is refused, with what to set, when its scheme is not http(s), a path
segment is `rest` (a pasted `/rest/api/2`), it has a query or fragment, or it is still the placeholder
`https://example.atlassian.net` the target templates ship. The check is in the agent's start, not the polar-env parser,
because the render's `--check` runs on targets that still hold the placeholder. The deployment, `QUERY_DATE` and the
placeholder email are refused there too (`agent.rs`, `start`), so a start-time refusal is one path (DECIDED, to review:
the render's `--check` does not catch a mistyped deployment or date; the parser would, but the refusal would then not be
the start-time one that pulled health reports).

## Where an operator sets it

- **URL, email, deployment, QUERY_DATE:** `workloads.jira-observer.env` in `infra/targets/<target>/config.nuon`
  (`{ name, value }` entries; `cluster-inputs.nuon` replaces entries of the same name). They become literals in the
  agent's document, ConfigMap `jira-observer-config` (`config.json`, the settings under `.sections[].variables[]` as
  `{ name, value }`). A change reaches the pod only after a render and apply (`up.nu`). Leave `JIRA_EMAIL` out for a Data
  Center token: the templates ship a placeholder email, and with any email the observer sends Basic (the placeholder
  itself is refused at start since 2026-11-01).
- **Reading anonymously:** `JIRA_ANONYMOUS` `true` in the same env list. NOT YET WIRED in infra: the catalog gives the
  pod `JIRA_TOKEN` from Secret `polar/jira-secret` (`infra/config/workloads/jira.nuon`), and a token beside the opt-in is
  refused, so an anonymous target needs that entry dropped (and no Secret); open, for the run against Apache.
- **Token:** `jira_token` in `infra/targets/<target>/user.sops.yaml` (preferred) or `secrets.sops.yaml`; `secrets.nu`
  makes Secret `polar/jira-secret` (key `token`) from it, and the pod reads it as `JIRA_TOKEN` (`secretKeyRef`). Store
  the RAW token: Kubernetes base64-encodes Secret data itself, and a token stored base64-encoded is sent encoded, which
  Data Center treats as no token at all (below). `secrets.nu` updates the Secret when the sops value changes and names
  the deployment to restart, and warns when a value looks base64-encoded (`infra/secrets-lib.nu`, INFRA.md); before
  2026-10-30 it skipped a Secret that existed, so a corrected token never reached the cluster.
- **By hand, for a test:** `read -rs JT; printf '%s' "$JT" | kubectl -n polar create secret generic jira-secret
  --from-file=token=/dev/stdin --dry-run=client -o yaml | kubectl apply -f -; unset JT`, then `kubectl -n polar rollout
  restart deploy/jira-observer` (an env var never changes in a running pod). The next `up.nu` puts the sops value back.

## What it does at start, and when Jira fails

1. Nothing calls Jira until the observer has registered with Cassini (`supervisor.rs`).
2. **What the site is** (`lib.rs`, `ensure_site`, since 2026-11-01): the issue and user observers, whose requests
   differ by deployment, ask `GET /rest/api/2/serverInfo` once; its `deploymentType` (`Cloud`, or anything else for
   Server or Data Center: all four public Data Centers answer `Server`) decides, over `JIRA_DEPLOYMENT` and the
   address, which are kept when it does not say (an error status, a body that does not decode, no `deploymentType`). A
   site that cannot be reached fails the pull, and the next asks again.
3. **The token** (`lib.rs`, `check_token`; skipped reading anonymously): `GET /rest/api/2/myself`. A 401 or 403, a success naming the
   anonymous user, or the `X-AUSERNAME: anonymous` header is refused with what to check (raw vs base64, and the auth
   mode: no `JIRA_EMAIL` for Data Center, `JIRA_EMAIL` for Cloud). Data Center answers a token it does not accept as an
   anonymous request: `/myself` 401 with `x-ausername: anonymous` and no `X-Seraph-LoginReason`, `/project` 200 with
   `[]`, `serverInfo` without a version where anonymous access is locked down.
4. A token that decodes from base64 to printable text is warned about once at start (`settings.rs`,
   `token_looks_base64`); a Data Center token, itself base64-looking, decodes to binary and does not trip it.
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
6. **Every Jira failure is retried, not fatal** (`lib.rs`, `settle`): an HTTP error, a connection, DNS or TLS
   failure, a timeout (connect 15 s, request 120 s, `supervisor.rs:56-61`), a decode error or a refused token is logged
   once per attempt (the host kept out of messages above debug) and retried after 30 s, doubling to 15 minutes
   (`lib.rs:55`, `RETRY`); after a success the next pull is the normal interval and the token is checked again after
   any failure. Each of the four observers keeps its own retry state. A configuration refusal at start (the URL,
   deployment, date and email checks) still ends the process, as any configuration error does. Since 2026-11-01:
   - **By kind** (`FailureKind`): `Transient` (unreachable, timed out, 5xx, 429, the state agent not saying),
     `Credentials` (401 or 403 once the token has been checked again in the same pull, a refused token, or any 401/403
     reading anonymously) and `Configuration` (404, 410, 400, or a body that does not decode from `/myself`,
     `/serverInfo` or `/field`). A 401/403 before the recheck is `Transient`: the next pull checks the token first. The
     observer keeps its last failure (`JiraObserverState::failure`) until a pull succeeds and logs `Credentials` and
     `Configuration` at error, with what to check. Every kind is still retried on the same backoff.
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
   - **The health hook** is `settle` (`lib.rs`), where the failure is set and cleared: once the builder's health API
     takes a domain failure, the observers are given the `HealthHandle` (the supervisor holds `resources.healthcheck`)
     in `JiraObserverArgs` and report `failure` there.

Before 2026-10-30 any Jira error ended the process: the pod's `RestartAllContainers` rule (deployments.md) restarted
the whole pod, the restart removed the container and its log, and the next start waited up to five minutes in
polar-init for the credentials agent's renewal rule (`TooSoon`, credentials-agent/common/src/issuance.rs). That is why
the failure on the Mac was invisible for a day. The last log lines of a failed container are now its termination
message (deployments.md, "termination message"), but the kubelet's removal record replaces it within about 2 s, so it
is seen only by someone watching the pod; a cause that outlives the container needs a file on a volume that survives
the restart (open, below).

## Checking a connection: `nu infra/jira-check.nu <target>`

Runs, from a short-lived pod on jira-observer's node pool, the requests the observer makes, with its document and its
Secret, and prints only facts safe to share: the URL's shape, the auth mode, the token's length and whether it looks
base64-encoded, the class of address it connected to (`100.64/10` as Zscaler Private Access gives, private, public), the
TLS verify result, and the HTTP status of `serverInfo`, `myself` (with whether it answered as anonymous) and `project`,
then what each pattern means. It never prints the host, URL, email or token (INFRA.md, `infra/tests/jira-check.nu`).

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

- The user picker: FIXED 2026-11-01 (`269ced0f`, below, 4): Server and Data Center are read by the user search.
- The cursor: BUILT 2026-10-30, and rebuilt the same day on the review (`soft-state.md`): the state actor asks, and the
  time is the system field `updated`, so a Jira in another language works too. Before, whether the full pull was done was held
  in memory, so every restart (about every half hour, with rejuvenation) pulled everything again or from the fixed
  `QUERY_DATE`, and a user moved `QUERY_DATE` by script as a hand-made cursor. Now the first pull after a start begins
  at the graph's newest issue; `QUERY_DATE` applies only to a graph with no issue. Seen on a cluster, the first build
  and the rebuild (soft-state.md, "Seen on a cluster" and "The rebuild, seen on a cluster"). Kept open: an issue updated
  during a walk by `startAt` moves to its end, so the issues after it shift back by one and one of them is skipped,
  as before (the window no longer drifts later under the walk since `5927d7ad`); and a graph whose issues were all
  written before jira-processor wrote `updated_ms` answers that none has it, so the first start after the upgrade
  pulls everything (or from `QUERY_DATE`) once, as before.
- A failed container's cause outlives it only if written to a volume that survives the in-place restart.
- Found 2026-11-01, not fixed: the processor makes `CHILD_OF` from the `parent` field's `name`
  (`process/src/issues.rs`, the `parent` branch), but Jira's `parent` carries `id`, `key`, `self` and `fields`, no
  `name`, so a sub-task's `CHILD_OF` is never written; the `key` is what names the parent issue.
- Found 2026-11-01: the users and groups observers cannot read anything anonymously on Apache: its user search answers
  401 to an anonymous request (and its user picker 404, "The user named '{0}' does not exist", for any query, so the
  review's 404 for `query=''` was the anonymous refusal, not the query), and its group picker answers no groups. Read
  anonymously, the users observer keeps a `Credentials` failure and retries on its backoff.

## The review before 3.0.0 (2026-11-01)

A read-only review of both agents, ranked, with two claims checked against a real Data Center (`issues.apache.org/jira`,
anonymous, one request each). Paths under `src/agents/jira/`. Fixed 2026-11-01 for 3.0.0 as each item says, against a
Data Center (the site is on-prem); the Cloud-only parts (ADF rich text, `accountId`-only users) are out of scope, and
nothing was made worse for Cloud. Verified with tests (wiremock sites paging as a Data Center does, and real issues
from Apache's Jira, `jira/common/fixtures/apache.rs`) and mutation checks; not yet run on a cluster.

1. **A start-time configuration refusal ends the process and the pod loops** (`observe/src/agent.rs:85`; the decided
   split, remaining-work.md "HEALTH IN THE GRAPH, PULLED", is unbuilt for start). NOT HERE: pulled health is the
   other workstream. What this one did: the refusals are precise (7, below), and the observers keep their failures by
   kind for health to carry (5).
2. **Changelog authors without an email drop the whole issue.** `JiraAuthor` requires `emailAddress`
   (`common/src/types.rs:79-91`); CHECKED: Apache's changelog authors carry `name`, `key`, `displayName`, `avatarUrls`,
   `active`, `timeZone`, `self` and no email, as any site that hides email does. `handle_issue`
   (`process/src/issues.rs:458-463`) logs a warning without the key and returns `Ok`: nothing written, nothing unhealthy.
   On Cloud also: users have only `accountId` (`JiraUser`, `JiraAuthor` require `name` and `key`), and
   `/rest/api/3/search/jql` returns rich text as ADF, which `FieldValue`'s five fixed tiers (`types.rs:118-188`) cannot
   hold, so a non-empty description fails to decode. FIXED (`dac5a02e`): every identity field of `JiraAuthor` and
   `JiraUser` is optional (an author is recorded by `key`, else `name`, else `accountId`; a user is keyed by `key`, else
   `name`, and one with neither is skipped, logged); an issue without `changelog` decodes; a payload that does not
   decode is logged at warn with its key (`6458ceb5`). ADF stays out of scope.
3. **A field name the graph refuses loses the issue for good.** Field names are sanitized, not validated
   (`process/src/issues.rs:184-199`); `validate_cypher_identifier` (`lib/src/graph/controller.rs:133-150`) refuses `.`,
   `:`, `'`, `&` and non-ASCII, and the issue's upsert is op 0 of one transaction, so one bad name (`Est. hours`,
   `Fälligkeitsdatum`) fails it, unretried. The cursor then passes over it, since newer issues are written. FIXED
   (`6458ceb5`): `keep_what_the_graph_accepts` takes out each property of each node and edge whose name the graph
   refuses, logged at warn with the issue's key; the rest is written. Apache's own field list has one such name,
   `Issue & fix info`.
4. **Pagination:** the group picker takes no `startAt` (`observe/src/groups.rs:66-94`): it re-fetches the first page
   and never sees later groups; the user picker's `query=''`: CHECKED, Apache answers 404, so the users observer backs
   off forever on that Data Center; a missing `total` (DC) or `isLast` (Cloud) ends a walk after one page, unlogged.
   The deployment guess (`observe/src/lib.rs:215-229`) can be replaced: `serverInfo` carries `deploymentType` (CHECKED:
   `Server` on all four public Data Centers); `/rest/api/2/search/jql` does not exist on Data Center (CHECKED: 404).
   FIXED (`269ced0f`): the group picker is paged as a Data Center pages it, by `exclude` (each request leaves out the
   groups read), to its total or a page with nothing new, bounded at a 6 KiB request (logged); Server and Data Center
   users are read from `/rest/api/2/user/search?username=.` (every user, active and inactive), paged by `startAt` to an
   empty page, Cloud keeps the picker; a missing `total` is logged and the walk goes on to an empty page, a missing
   `isLast` is logged and the page token decides. CHECKED 2026-11-01: Apache's user picker answers 404 to an anonymous
   request for any query (`query=a` too), and its user search 401: the 404 was the anonymous refusal. The deployment
   comes from `serverInfo` (`6f424dba`, 7).
5. **Every non-2xx is one retry** (`observe/src/lib.rs:362-378`, `settle` :436): 404, 410, a refused token, a malformed
   body back off forever and never reach health; 429's `Retry-After` is ignored; a failure on page N restarts at page 0.
   FIXED (`5927d7ad`): failures are classified (`FailureKind`: `Transient`, `Credentials`, `Configuration`) and kept by
   each observer until a success, `Retry-After` is honoured on 429 and 503, and a walk resumes at the page that failed
   (above, "What it does at start", 6). Still retried on the same backoff; the classification reaches health once the
   pulled-health API takes it (the hook is `settle`).
6. **The processor's shutdown drops queued writes:** `stop_and_wait` (`process/src/supervisor.rs:286-305`) is served
   before ordinary messages, so ops still in the graph controller's mailbox are lost at each rejuvenation;
   `drain_and_wait` keeps them. Writes while Neo4j is down are dropped, not retried. MERGE keeps every write repeatable,
   but never removes an old `ASSIGNED` edge or a cleared field's property. FIXED (`85b624ee`): the shutdown drains
   the graph controller (`drain_and_wait`, `stop_graph_controller`), so every op already queued is written before it
   stops. Not changed: writes while Neo4j is down are still dropped, and MERGE still never removes an old edge or
   property.
7. **Start validation lets through:** a mistyped `JIRA_DEPLOYMENT` (falls back to the guess), `QUERY_DATE` unchecked and
   unquoted in the JQL (a time gives a 400, retried forever), the shipped placeholder `JIRA_EMAIL`
   `jira-user@example.org` (a Data Center then gets Basic auth, refused forever). FIXED (`6f424dba`): each is refused
   at start with what to set; `QUERY_DATE` is said back in one form and quoted in the JQL; the deployment type is
   `serverInfo`'s `deploymentType`, the setting and the address only where it does not say.
8. **English display names:** the processor finds assignee, reporter, creator, type and project by display name
   (`process/src/issues.rs:236-278`), so on a site in another language those edges are not written (the cursor is
   language-safe; the "another language works too" above is about the cursor only). FIXED (`a38efe97`): the observer
   moves those five system fields out of `fields` to beside them, as `polar_system`, by system id (`/field`'s
   `schema.system`, or the field id that is the system name), as it passes `updated`; the processor makes ASSIGNED,
   REPORTED, CREATED, HAS_TYPE and PART_OF from there only (`system_field_ops`). Found on Apache: a custom field named
   "Project" (`customfield_12312823`) took the system field's place in the renaming whenever it came first, so the
   display name was not safe in English either.
9. **Tests:** no multi-page walk, no 429, no realistic issue JSON through `JiraIssue`, no graph write, nothing end to
   end; the fake site in `observe/tests/observers.rs` serves issues without `changelog`, and the u7c fake is not
   committed. FIXED (`118c1427` and each fix's own tests): real issues from Apache's Jira (`jira/common/fixtures/apache.rs`,
   its comment says how they were read and trimmed) through the observer's preparation, pinned, and through
   `JiraIssue` and the processor's ops, every op compiled as the graph controller does; multi-page walks of issues,
   users and groups; 429 and 503 with `Retry-After`; resume at the failed page; a refused field name; anonymous reading.
   Property tests: a `QUERY_DATE` round trip, a refused name never failing an issue, system fields matched by id
   whatever their display names, a resumed walk never reaching back less than where it began. Still not committed:
   the u7c fake; still nothing end to end against a graph.

The stale lines this section listed are corrected above: egress is built (`polar::egress`; `PROXY_CA_CERT` is gone,
the section is `POLAR_EGRESS_*`), and the cursor was seen on a cluster.

Anonymous reading (`6fbe0a4b`): `JIRA_ANONYMOUS=true`, an explicit opt-in (the table above). It sends no
`Authorization` header and makes no token check, `JIRA_TOKEN` is not required and refused beside it, and the start log
says the observer reads only what the site shows the public. For the run against Apache's Jira.

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
11. **An issue without `changelog`, and a user without a key, decode**; the user is keyed by its name, or skipped.
