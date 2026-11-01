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

# The Jira agents: configuration, connection and failure (2026-10-30, as built)

jira-observer polls a Jira site for projects, groups, users and issues and publishes them to Cassini; jira-processor
writes them into the graph. This page is what an operator needs to connect a site and what the observer does when it
cannot. It was written after a real Data Center site was connected on 2026-10-30 (minikube on a managed Mac) and every
step that failed there was fixed; the fixes are in the sections below.

## What the observer reads

Six polar-env variables, section `jira` (`jira/observe/src/settings.rs`), all static (the ConfigurationActor refuses
changes to them, signed-configuration.md):

| Variable | | What it does |
| --- | --- | --- |
| `JIRA_URL` | required | the site's root: `https://jira.example.org` or, with a context path, `https://host/jira` |
| `JIRA_TOKEN` | required, secret | the token, from Secret `polar/jira-secret`, key `token` |
| `JIRA_EMAIL` | optional | set: Basic `email:token` (a Jira Cloud API token); unset: `Bearer <token>` (a Data Center or Server personal access token) (`lib.rs`, `JiraAuth::from_parts`) |
| `JIRA_DEPLOYMENT` | optional | `cloud`, `server` or `datacenter`; unset, Cloud only for an `.atlassian.net` address |
| `PROXY_CA_CERT` | optional | a PEM path; nothing can mount one yet (below: private CAs and proxies) |
| `QUERY_DATE` | optional | when the graph has no issue yet, the first issue pull asks only for `updated > QUERY_DATE`; once it has one, the first pull starts from the graph instead (below) |

Every endpoint is the site root plus `/rest/api/...`, so `normalise_jira_url` (`settings.rs:115`) is applied at start:
trailing slashes are dropped, and an address is refused, with what to set, when its scheme is not http(s), a path
segment is `rest` (a pasted `/rest/api/2`), it has a query or fragment, or it is still the placeholder
`https://example.atlassian.net` the target templates ship. The check is in the agent's start, not the polar-env parser,
because the render's `--check` runs on targets that still hold the placeholder.

## Where an operator sets it

- **URL, email, deployment, QUERY_DATE:** `workloads.jira-observer.env` in `infra/targets/<target>/config.nuon`
  (`{ name, value }` entries; `cluster-inputs.nuon` replaces entries of the same name). They become literals in the
  agent's document, ConfigMap `jira-observer-config` (`config.json`, the settings under `.sections[].variables[]` as
  `{ name, value }`). A change reaches the pod only after a render and apply (`up.nu`). Leave `JIRA_EMAIL` out for a Data
  Center token: the templates ship a placeholder email, and with any email the observer sends Basic.
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
2. **The token first** (`lib.rs:352`, `check_token`): `GET /rest/api/2/myself`. A 401 or 403, a success naming the
   anonymous user, or the `X-AUSERNAME: anonymous` header is refused with what to check (raw vs base64, and the auth
   mode: no `JIRA_EMAIL` for Data Center, `JIRA_EMAIL` for Cloud). Data Center answers a token it does not accept as an
   anonymous request: `/myself` 401 with `x-ausername: anonymous` and no `X-Seraph-LoginReason`, `/project` 200 with
   `[]`, `serverInfo` without a version where anonymous access is locked down.
3. A token that decodes from base64 to printable text is warned about once at start (`settings.rs:155`,
   `token_looks_base64`); a Data Center token, itself base64-looking, decodes to binary and does not trip it.
4. Projects, groups, users and issues each pull at once and then every 1800 s (`agent.rs:39`). **Before its first
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
5. **Every Jira failure is retried, not fatal** (`lib.rs:403`, `settle`): an HTTP error, a connection, DNS or TLS
   failure, a timeout (connect 15 s, request 120 s, `supervisor.rs:56-61`), a decode error or a refused token is logged
   once per attempt (the host kept out of messages above debug) and retried after 30 s, doubling to 15 minutes
   (`lib.rs:55`, `RETRY`); after a success the next pull is the normal interval and the token is checked again after
   any failure. Each of the four observers keeps its own retry state. A configuration refusal at start (the URL checks)
   still ends the process, as any configuration error does.

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
  Neither works today: nothing mounts `PROXY_CA_CERT`'s file (`infra/resolve.nu`, `proxy_ca_cert: null`), and when it is
  set the client switches to rustls with webpki roots and drops the image's bundle. Decided 2026-10-30, being built: an
  optional `cluster.egress` setting (`proxy`, `no_proxy`, `ca_file`), declared polar-env variables for the agents that
  call outside services (jira-observer, git-repo-observer, the OCI resolver), the CA added beside the system roots, and
  in-cluster names never proxied; a target that sets nothing renders the same pods (egress.md, when built).

## Open

- The user picker sends `query=''` and may repeat pages (`users.rs`; remaining-work.md); not changed, because what an
  empty query lists differs between Jira versions.
- The cursor: BUILT 2026-10-30, and rebuilt the same day on the review (`soft-state.md`): the state actor asks, and the
  time is the system field `updated`, so a Jira in another language works too. Before, whether the full pull was done was held
  in memory, so every restart (about every half hour, with rejuvenation) pulled everything again or from the fixed
  `QUERY_DATE`, and a user moved `QUERY_DATE` by script as a hand-made cursor. Now the first pull after a start begins
  at the graph's newest issue; `QUERY_DATE` applies only to a graph with no issue. Not yet seen on a cluster. Kept
  open: pagination by `startAt` can skip an issue updated during a pull, as before; and a graph whose issues were all written before jira-processor wrote `updated_ms` answers that
  none has it, so the first start after the upgrade pulls everything (or from `QUERY_DATE`) once, as before.
- A failed container's cause outlives it only if written to a volume that survives the in-place restart.

## The review before 3.0.0 (2026-11-01)

A read-only review of both agents, ranked, with two claims checked against a real Data Center (`issues.apache.org/jira`,
anonymous, one request each). Paths under `src/agents/jira/`. Nothing here is fixed yet.

1. **A start-time configuration refusal ends the process and the pod loops** (`observe/src/agent.rs:85`; the decided
   split, remaining-work.md "HEALTH IN THE GRAPH, PULLED", is unbuilt for start).
2. **Changelog authors without an email drop the whole issue.** `JiraAuthor` requires `emailAddress`
   (`common/src/types.rs:79-91`); CHECKED: Apache's changelog authors carry `name`, `key`, `displayName`, `avatarUrls`,
   `active`, `timeZone`, `self` and no email, as any site that hides email does. `handle_issue`
   (`process/src/issues.rs:458-463`) logs a warning without the key and returns `Ok`: nothing written, nothing unhealthy.
   On Cloud also: users have only `accountId` (`JiraUser`, `JiraAuthor` require `name` and `key`), and
   `/rest/api/3/search/jql` returns rich text as ADF, which `FieldValue`'s five fixed tiers (`types.rs:118-188`) cannot
   hold, so a non-empty description fails to decode.
3. **A field name the graph refuses loses the issue for good.** Field names are sanitized, not validated
   (`process/src/issues.rs:184-199`); `validate_cypher_identifier` (`lib/src/graph/controller.rs:133-150`) refuses `.`,
   `:`, `'`, `&` and non-ASCII, and the issue's upsert is op 0 of one transaction, so one bad name (`Est. hours`,
   `Fälligkeitsdatum`) fails it, unretried. The cursor then passes over it, since newer issues are written.
4. **Pagination:** the group picker takes no `startAt` (`observe/src/groups.rs:66-94`): it re-fetches the first page
   and never sees later groups; the user picker's `query=''`: CHECKED, Apache answers 404, so the users observer backs
   off forever on that Data Center; a missing `total` (DC) or `isLast` (Cloud) ends a walk after one page, unlogged.
   The deployment guess (`observe/src/lib.rs:215-229`) can be replaced: `serverInfo` carries `deploymentType` (CHECKED:
   `Server` on all four public Data Centers); `/rest/api/2/search/jql` does not exist on Data Center (CHECKED: 404).
5. **Every non-2xx is one retry** (`observe/src/lib.rs:362-378`, `settle` :436): 404, 410, a refused token, a malformed
   body back off forever and never reach health; 429's `Retry-After` is ignored; a failure on page N restarts at page 0.
6. **The processor's shutdown drops queued writes:** `stop_and_wait` (`process/src/supervisor.rs:286-305`) is served
   before ordinary messages, so ops still in the graph controller's mailbox are lost at each rejuvenation;
   `drain_and_wait` keeps them. Writes while Neo4j is down are dropped, not retried. MERGE keeps every write repeatable,
   but never removes an old `ASSIGNED` edge or a cleared field's property.
7. **Start validation lets through:** a mistyped `JIRA_DEPLOYMENT` (falls back to the guess), `QUERY_DATE` unchecked and
   unquoted in the JQL (a time gives a 400, retried forever), the shipped placeholder `JIRA_EMAIL`
   `jira-user@example.org` (a Data Center then gets Basic auth, refused forever).
8. **English display names:** the processor finds assignee, reporter, creator, type and project by display name
   (`process/src/issues.rs:236-278`), so on a site in another language those edges are not written (the cursor is
   language-safe; the "another language works too" above is about the cursor only).
9. **Tests:** no multi-page walk, no 429, no realistic issue JSON through `JiraIssue`, no graph write, nothing end to
   end; the fake site in `observe/tests/observers.rs` serves issues without `changelog`, and the u7c fake is not
   committed.

Stale here: egress is built (`polar::egress`; `PROXY_CA_CERT` is gone, the section is `POLAR_EGRESS_*`), and the cursor
was seen on a cluster (soft-state.md, "Seen on a cluster" and "The rebuild, seen on a cluster").
