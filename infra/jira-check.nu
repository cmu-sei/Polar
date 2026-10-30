#!/usr/bin/env nu

# ACE Tool
#
# Copyright 2026 Carnegie Mellon University.
#
# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
# FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
# EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
# PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
# MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
# TRADEMARK, OR COPYRIGHT INFRINGEMENT.
#
# Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
# for full terms.
#
# [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
# distribution.  Please see Copyright notice for non-US Government use and distribution.
#
# This Software includes and/or makes use of Third-Party Software each subject to its own license.
#
# DM26-0670

# infra/jira-check.nu
#
# Whether jira-observer can reach its Jira site and is let in, asked from inside the cluster, and said in facts that
# are safe to share: no hostname, URL, address, email or token is printed. A short-lived pod in `polar`, on
# jira-observer's node pool, is given jira-observer's settings from the same sources (the literal values in its
# ConfigMap's document, and the token from its Secret as an environment variable, never on a command line) and asks
# Jira as the agent does: the URL as it is, the same authentication. The pod runs the nu-init image the cluster
# already runs (Neo4j's init container's), and this file, from a ConfigMap. The pod and its ConfigMap are deleted at
# the end, whether the check succeeded, failed or was interrupted; a pod an interrupted run left behind ends by its
# deadline and is deleted by the next run.
#
# It prints: the shape of JIRA_URL; the auth mode (Basic with JIRA_EMAIL, Bearer without, as the agent chooses); the
# token's length, whether it has whitespace and whether it looks base64-encoded; the class of the address the pod
# connected to (100.64/10 "ZPA-style", RFC 1918 "private", "public"), which nu cannot say, so openssl's when it
# connects and the pod's socket's when a connection never completes; and the HTTP status of serverInfo, myself
# (whether Jira took the caller as anonymous) and project (how many it returned). Then what those facts mean.
#
# Self-contained: the pod runs this file, so it uses no other module.
#
#   nu infra/jira-check.nu <target>

const SELF = (path self)
const INFRA = (path self | path dirname)

const POLAR = "polar"
const WORKLOAD = "jira-observer"
const CONFIG_MAP = "jira-observer-config"
# Where jira-observer's token comes from when its Deployment does not say (it does: the catalog's `env` entry).
const TOKEN_SECRET = { name: "jira-secret", key: "token" }
# The image that has nu: nu-init, which the cluster runs as Neo4j's init container. Found by name in what runs.
const NU_IMAGE = "polar-nu-init"
const SCRATCH_LABEL = "polar.dev/scratch"
const SCRATCH_VALUE = "jira-check"
# The user the pod runs as: the image's own.
const POD_UID = 65532
# How long the pod may run, and how long this waits for it, its image's pull included.
const POD_DEADLINE_SECS = 120
const WAIT_LIMIT = 180sec
# Who Jira says it is, who it takes the caller for, and the projects the agent lists
# (src/agents/jira/observe/src/projects.rs).
const ENDPOINTS = ["/rest/api/2/serverInfo" "/rest/api/2/myself" "/rest/api/2/project"]
const REQUEST_LIMIT = 15sec
# How long the connection for the address may take: openssl has no connect timeout of its own.
const CONNECT_LIMIT = 10sec

# ── The settings (pure) ──────────────────────────────────────────────────────

# The host of a URL, lowercased, or null when it has none.
export def url_host [url: string] {
    let parsed = (try { $url | str trim | url parse } catch { null })
    let host = ($parsed.host? | default "")
    if ($host | is-empty) { null } else { $host | str lowercase }
}

# The port a URL connects to: its own, or its scheme's.
export def url_port [url: string] {
    let parsed = (try { $url | str trim | url parse } catch { null })
    let port = ($parsed.port? | default "")
    if ($port | is-not-empty) { $port | into int } else if ($parsed.scheme? | default "") == "http" { 80 } else { 443 }
}

# A host that is an example, never a real site: the target's placeholder (example.atlassian.net) and RFC 2606's names.
export def is_example_host [host: string] {
    let h = ($host | str lowercase | str trim --right --char ".")
    ($h == "example.atlassian.net") or ($h =~ '(^|\.)example(\.(com|net|org))?$')
}

# What the URL looks like, without the URL. The agent appends `/rest/api/...` to it as it is.
export def url_facts [url: string] {
    let u = ($url | str trim)
    let host = (url_host $u)
    let path = (try { $u | url parse | get path } catch { "" })
    {
        set: ($u | is-not-empty)
        https: ($u | str lowercase | str starts-with "https://")
        trailing_slash: ($u | str ends-with "/")
        rest: ($path | str lowercase | str contains "/rest")
        atlassian: ($host != null and ($host == "atlassian.net" or ($host | str ends-with ".atlassian.net")))
        placeholder: ($host != null and (is_example_host $host))
    }
}

# The agent's choice: Basic (Atlassian Cloud's email and API token) when JIRA_EMAIL is set, a bearer token otherwise.
export def auth_mode [email: string] {
    if ($email | is-empty) { "Bearer" } else { "Basic" }
}

# What JIRA_DEPLOYMENT says, by the agent's reading of it (src/agents/jira/observe/src/lib.rs, `JiraDeployment`).
export def deployment_fact [value: string] {
    let v = ($value | str lowercase)
    if ($v | is-empty) { "unset (the agent takes Cloud for an atlassian.net address, Server otherwise)" } else if $v in ["cloud" "server" "datacenter"] { $v } else { "set to something the agent does not recognise (it then follows the address)" }
}

# Whether `s` looks like base64 of text: the standard alphabet, a whole number of quanta, at least 16 characters, and
# decoding to printable ASCII without whitespace (a trailing newline allowed, as `echo token | base64` leaves). A raw
# Atlassian token has `-` and `_`, outside the alphabet; a Data Center token decodes to bytes that are not text.
export def looks_base64 [s: string] {
    let t = ($s | str trim)
    let n = ($t | str length)
    if $n < 16 or ($n mod 4) != 0 or not ($t =~ '^[A-Za-z0-9+/]+={0,2}$') { return false }
    let hex = (try { $t | decode base64 | encode hex } catch { null })
    if $hex == null { return false }
    let bytes = ($hex | split chars | chunks 2 | each {|c| $c | str join | into int --radix 16 })
    let text = ($bytes | reverse | skip while {|b| $b == 10 or $b == 13 } | reverse)
    ($text | is-not-empty) and ($text | all {|b| $b >= 33 and $b <= 126 })
}

export def token_facts [token: string] {
    { length: ($token | str length), whitespace: ($token =~ '\s'), base64: (looks_base64 $token) }
}

# The literal values of the Jira settings in jira-observer's document (`.sections[].variables[]`, each { name, value }):
# a text, or "" for one unset or given from elsewhere (`{ env: ... }`).
export def settings_of_document [doc: record] {
    let vars = ($doc.sections? | default [] | each {|s| $s.variables? | default [] } | flatten)
    let literal = {|name|
        let v = ($vars | where name == $name | get -o 0.value)
        if ($v | describe) == "string" { $v } else { "" }
    }
    { url: (do $literal "JIRA_URL"), email: (do $literal "JIRA_EMAIL"), deployment: (do $literal "JIRA_DEPLOYMENT") }
}

# ── The address (pure) ───────────────────────────────────────────────────────

# The class of an address: "ZPA-style" (100.64/10, the shared space Zscaler Private Access answers from), "private"
# (RFC 1918, or IPv6 ULA), "loopback", "link-local", "public"; "no connection" for none; "unknown" for what is not one.
export def address_class [address: any] {
    if $address == null or ($address | is-empty) { return "no connection" }
    let a = ($address | str lowercase | str trim --left --char "[" | str trim --right --char "]" | str replace --regex '^::ffff:' '')
    let v4 = ($a | parse --regex '^(?<a>\d{1,3})\.(?<b>\d{1,3})\.(?<c>\d{1,3})\.(?<d>\d{1,3})$')
    if ($v4 | is-not-empty) {
        let o = ($v4 | first | values | each {|x| $x | into int })
        if ($o | any {|x| $x > 255 }) { return "unknown" }
        let x = ($o | get 0)
        let y = ($o | get 1)
        if $x == 100 and $y >= 64 and $y <= 127 { return "ZPA-style" }
        if $x == 10 or ($x == 172 and $y >= 16 and $y <= 31) or ($x == 192 and $y == 168) { return "private" }
        if $x == 127 { return "loopback" }
        if $x == 169 and $y == 254 { return "link-local" }
        return "public"
    }
    if ($a =~ '^[0-9a-f:]+$') and ($a | str contains ":") {
        if $a == "::1" or ($a =~ '^(0{1,4}:){7}0{0,3}1$') { return "loopback" }
        if $a =~ '^f[cd]' { return "private" }
        if $a =~ '^fe[89ab]' { return "link-local" }
        return "public"
    }
    "unknown"
}

# The address openssl says it connected to ("Connecting to <address>", on a connection made), or null.
export def openssl_address [output: string] {
    $output | parse --regex 'Connecting to (?<a>\S+)' | get -o 0.a
}

# openssl's verdict on the certificate chain, as "<code> (<reason>)", or null.
export def openssl_verify [output: string] {
    let v = ($output | parse --regex 'Verify return code: (?<c>\d+) \((?<r>[^)]*)\)' | get -o 0)
    if $v == null { null } else { $"($v.c) \(($v.r)\)" }
}

# An address as /proc/net/tcp{,6} writes it: IPv4 as one little-endian word, IPv6 as four.
export def hex_address [hex: string] {
    let pairs = {|h| $h | split chars | chunks 2 | each {|c| $c | str join } }
    if ($hex | str length) == 8 {
        return (do $pairs $hex | reverse | each {|b| $b | into int --radix 16 | into string } | str join ".")
    }
    let bytes = ($hex | split chars | chunks 8 | each {|w| do $pairs ($w | str join) | reverse } | flatten)
    let groups = ($bytes | chunks 2 | each {|g| $g | str join | str lowercase })
    if ($groups | first 6) == ["0000" "0000" "0000" "0000" "0000" "ffff"] {
        return ($bytes | last 4 | each {|b| $b | into int --radix 16 | into string } | str join ".")
    }
    $groups | each {|g| let t = ($g | str trim --left --char "0"); if ($t | is-empty) { "0" } else { $t } } | str join ":"
}

# The remote addresses of the sockets in a /proc/net/tcp{,6} listing that go to `port`, a connection being made
# (SYN sent, which is all a connection to nowhere ever shows) or made. A listening socket's remote port is 0.
export def proc_net_remotes [listing: string, port: int] {
    $listing | lines | skip 1 | each {|l| $l | str trim | split row --regex '\s+' } | where {|f| ($f | length) > 3 }
    | each {|f|
        let remote = ($f | get 2 | split row ":")
        { address: ($remote | get 0), port: ($remote | get 1 | into int --radix 16) }
    }
    | where port == $port
    | each {|r| hex_address $r.address }
    | uniq
}

# ── Errors and masking (pure) ────────────────────────────────────────────────

# The kind of a failed request, from nu's error (its message and its debug form, e.g. "DNS Error", or "I/O error"
# with `Io(... kind: Std(ConnectionRefused ...`).
export def error_kind [msg: string, debug: string] {
    let s = ($"($msg) ($debug)" | str lowercase)
    if ($s =~ 'dns|failed to lookup|name or service not known|nodename nor servname') { return "DNS" }
    if ($s =~ 'connectionrefused|connection refused') { return "connection refused" }
    if ($s =~ 'timedout|timed out|timeout') { return "timeout" }
    if ($s =~ 'invaliddata|certificate|tls|ssl|handshake') { return "TLS" }
    if ($s =~ 'unreachable') { return "unreachable" }
    if ($s =~ 'connectionreset|connection reset|connectionaborted|brokenpipe') { return "connection reset" }
    "other"
}

# The one word of nu's error that says more (`NoName`, `ConnectionRefused`, `InvalidData`), never more than that word.
export def error_detail [debug: string] {
    $debug | parse --regex 'kind: (?:Std\()?(?<k>[A-Za-z]+)' | get -o 0.k
}

def regex_escape [s: string] { $s | str replace --all --regex '([\\.^$|?*+()\[\]{}])' '\$1' }

# A line with every setting it could hold replaced: the token and the credential made from it, the URL, the email
# and the host (whatever their case), then anything else shaped like a URL, an email or an address.
export def mask [text: string, secrets: record] {
    let named = (
        [
            { value: ($secrets.credential? | default ""), label: "JIRA-TOKEN" }
            { value: ($secrets.token? | default ""), label: "JIRA-TOKEN" }
            { value: ($secrets.url? | default "" | str trim), label: "JIRA-URL" }
            { value: ($secrets.email? | default ""), label: "JIRA-EMAIL" }
            { value: ($secrets.host? | default ""), label: "JIRA-HOST" }
        ] | where {|s| $s.value | is-not-empty }
    )
    mut t = $text
    for s in $named {
        $t = ($t | str replace --all --regex $"\(?i\)(regex_escape $s.value)" $s.label)
    }
    $t
    | str replace --all --regex '(?i)\b[a-z][a-z0-9+.-]*://\S+' 'URL'
    | str replace --all --regex '[^\s@]+@[^\s@]+' 'EMAIL'
    | str replace --all --regex '\d{1,3}(\.\d{1,3}){3}' 'IP'
    | str replace --all --regex '(?i)(?<![\w:])(?:[0-9a-f]{0,4}:){2,7}[0-9a-f]{1,4}(?![\w:])' 'IP'
}

# ── What the facts mean (pure) ───────────────────────────────────────────────

# One endpoint's facts from nu's `http get --full` answer.
export def endpoint_facts [path: string, response: record] {
    let headers = ($response.headers?.response? | default [])
    let user = ($headers | where {|h| ($h.name | str lowercase) == "x-ausername" } | get -o 0.value)
    let body = if ($response.body? | describe) == "string" { try { $response.body | from json } catch { null } } else { $response.body? }
    let status = $response.status
    {
        path: $path
        status: $status
        error: null
        detail: null
        anonymous: (if ($path | str ends-with "/myself") and $user != null { ($user | str lowercase) == "anonymous" } else { null })
        projects: (if ($path | str ends-with "/project") and $status == 200 and (($body | describe) =~ '^(list|table)') { $body | length } else { null })
        deployment_type: (if ($path | str ends-with "/serverInfo") and (($body | describe) =~ '^record') and (($body.deploymentType? | default "") =~ '^[A-Za-z]+$') { $body.deploymentType } else { null })
    }
}

def yes_no [b: bool] { if $b { "yes" } else { "no" } }

def endpoint_line [e: record] {
    let name = ($e.path | path basename)
    if $e.error != null {
        let detail = if $e.detail == null { "" } else { $" \(($e.detail)\)" }
        return $"  ($name | fill -w 11)($e.error)($detail)"
    }
    mut parts = [$"HTTP ($e.status)"]
    if ($name == "myself") {
        $parts = ($parts | append $"x-ausername: (if $e.anonymous == null { 'absent' } else if $e.anonymous { 'anonymous' } else { 'a user' })")
    }
    if $e.projects != null { $parts = ($parts | append $"($e.projects) projects") }
    if $e.deployment_type != null { $parts = ($parts | append $"deploymentType ($e.deployment_type)") }
    $"  ($name | fill -w 11)($parts | str join ', ')"
}

# The facts, one line each.
export def fact_lines [facts: record] {
    let u = $facts.url
    let t = $facts.token
    [
        $"  nu         ($facts.nu)"
        $"  JIRA_URL   set: (yes_no $u.set), https: (yes_no $u.https), trailing slash: (yes_no $u.trailing_slash), contains /rest: (yes_no $u.rest), atlassian.net: (yes_no $u.atlassian), example placeholder: (yes_no $u.placeholder)"
        $"  deployment ($facts.deployment)"
        $"  auth       ($facts.auth)(if $facts.auth == 'Basic' { $', JIRA_EMAIL the example placeholder: (yes_no $facts.email_placeholder)' } else { ', JIRA_EMAIL unset' })"
        $"  token      length ($t.length), whitespace: (yes_no $t.whitespace), looks base64-encoded: (yes_no $t.base64)"
        $"  address    ($facts.address.class) \(($facts.address.source)\)"
        $"  TLS        (if $facts.tls_verify == null { 'not checked' } else { $'openssl verify ($facts.tls_verify)' })"
        ...($facts.endpoints | each {|e| endpoint_line $e })
    ]
}

# What the facts mean, one line for each pattern they match.
export def interpret [facts: record] {
    let u = $facts.url
    let t = $facts.token
    let answered = ($facts.endpoints | where status != null)
    let failed = ($facts.endpoints | where error != null and error != "not tried")
    let statuses = ($answered | get status)
    let myself = ($facts.endpoints | where {|e| $e.path | str ends-with "/myself" } | get -o 0)
    let project = ($facts.endpoints | where {|e| $e.path | str ends-with "/project" } | get -o 0)
    mut out = []
    if not $u.set {
        return ["JIRA_URL is not set: set it in the target's config (workloads.jira-observer.env)."]
    }
    if $u.placeholder {
        $out = ($out | append "JIRA_URL is still the example placeholder: set the target's JIRA_URL (targets/<target>/config.nuon, workloads.jira-observer.env) to the Jira site and deploy again.")
    }
    if not $u.https { $out = ($out | append "JIRA_URL is not https: Jira Cloud answers only https, and a token sent over http is in the clear.") }
    if $u.trailing_slash { $out = ($out | append "JIRA_URL ends with '/': the agent appends /rest/api/..., so it asks for '//rest/...'. Drop the slash.") }
    if $u.rest { $out = ($out | append "JIRA_URL contains /rest: give the site's address alone; the agent appends the REST path itself.") }
    if $facts.auth == "Basic" and not $u.atlassian {
        $out = ($out | append "JIRA_EMAIL is set, so the agent uses Basic authentication, which is Atlassian Cloud's, and the host is not atlassian.net: for Server or Data Center with a personal access token, unset JIRA_EMAIL (Bearer).")
    }
    if $facts.auth == "Bearer" and $u.atlassian {
        $out = ($out | append "The host is atlassian.net and JIRA_EMAIL is unset, so the agent sends a bearer token: Atlassian Cloud wants Basic, the account's email with an API token. Set JIRA_EMAIL.")
    }
    if $facts.auth == "Basic" and $facts.email_placeholder { $out = ($out | append "JIRA_EMAIL is still the example placeholder: set it to the account the token belongs to.") }
    if $t.length == 0 { $out = ($out | append "The token is empty: put it in the Secret's key the Deployment names.") }
    if $t.whitespace { $out = ($out | append "The token has whitespace (often a newline from `echo` when the Secret was made): make the Secret again from the token alone.") }
    if $t.base64 { $out = ($out | append "The token decodes as base64 to text: it may be stored encoded twice (kubectl encodes a --from-literal value itself), or be base64 of email:token. Store the raw token.") }
    if (401 in $statuses) or ($myself.anonymous? == true) {
        $out = ($out | append "Jira did not accept the token (401, or the caller taken as anonymous): check that the Secret holds the raw token, not base64 of it, and that the auth mode fits the site (Basic with JIRA_EMAIL for Cloud, Bearer for a Server or Data Center personal access token).")
    }
    if 403 in $statuses { $out = ($out | append "Jira refused the account (403): it lacks the permission, or Server asks for a CAPTCHA after failed logins (log in once in a browser).") }
    if (404 in $statuses) and not $u.placeholder { $out = ($out | append "Jira answered 404: there is no Jira REST API at this address. Check the URL's path (a Server site may live under a context path, such as /jira).") }
    let kinds = ($failed | get error | uniq)
    let cls = $facts.address.class
    if "DNS" in $kinds {
        $out = ($out | append "The pod cannot resolve JIRA-HOST: the cluster's DNS has no answer for it. A private name needs the cluster's DNS (CoreDNS) to forward its domain to a resolver that knows it (for ZPA, the resolver its connector uses).")
    }
    if ("connection refused" in $kinds) or ("timeout" in $kinds) or ("unreachable" in $kinds) or ("connection reset" in $kinds) {
        let where = if $cls == "ZPA-style" { " The name resolves to a ZPA-style address (100.64/10): the node must reach ZPA's connector, which a pod usually cannot." } else if $cls == "no connection" { "" } else { $" The address is ($cls)." }
        $out = ($out | append $"The pod resolved JIRA-HOST but could not connect \(($kinds | where $it != 'DNS' | str join ', ')\).($where) Check the nodes' egress \(firewall, VPN, proxy\).")
    }
    if "TLS" in $kinds {
        $out = ($out | append "TLS failed: the certificate is not trusted by the pod's CA bundle, or something between presents its own (a proxy or ZPA inspecting TLS). The agent trusts PROXY_CA_CERT besides the system's; openssl's verify code above says which.")
    }
    if "other" in $kinds { $out = ($out | append "A request failed in a way this check does not recognise; the kind above is nu's.") }
    # Every answer a 200, and nothing above said: a failed request, or an anonymous caller, was.
    let good = ($statuses | is-not-empty) and ($statuses | all {|s| $s == 200 })
    if $good and ($out | is-empty) {
        let n = ($project.projects? | default 0)
        if $n == 0 {
            $out = ($out | append "Jira let the account in, but it sees no projects: give it the Browse projects permission on the ones to observe.")
        } else {
            $out = ($out | append $"All good: the address, the token and the auth mode are accepted, and the account sees ($n) projects.")
        }
    }
    if ($out | is-empty) { $out = ($out | append "Nothing this check knows explains the answers above.") }
    $out
}

# Everything the check says: the facts, then what they mean.
export def report_lines [facts: record] {
    ["facts:" ...(fact_lines $facts) "what they mean:" ...(interpret $facts | each {|l| $"  - ($l)" })]
}

# What the pod prints: the report, each line masked of the URL, the host, the email and the credential. Not of the
# token: nothing the report says comes from it, so it could be in a line only as a word the check says anyway, and
# masking that word would show it (a token "placeholder" would turn "example placeholder" into "example JIRA-TOKEN").
export def printed [facts: record, secrets: record] {
    let shown = ($secrets | reject -o token)
    report_lines $facts | each {|l| mask $l $shown }
}

# ── In the pod ───────────────────────────────────────────────────────────────

# The address the pod connects to for the host: openssl's word when it connects, otherwise the socket's own while the
# connection is being made (/proc/net/tcp, which a connection to nowhere shows as SYN sent). openssl's verdict on the
# certificate besides. Its output is read, never printed.
def connect_facts [host: string, port: int] {
    let out = ($env.HOME | path join "openssl.txt")
    let tag = (random int 1..1000000)
    let job = (job spawn { try { "" | ^openssl s_client -connect $"($host):($port)" -servername $host o+e> $out }; "done" | job send 0 --tag $tag })
    let started = (date now)
    mut seen = []
    mut finished = false
    while ((date now) - $started) < $CONNECT_LIMIT {
        let listing = (["/proc/net/tcp" "/proc/net/tcp6"] | where {|f| $f | path exists } | each {|f| open --raw $f } | str join "\n")
        $seen = ($seen | append (proc_net_remotes $listing $port) | uniq)
        if (try { job recv --tag $tag --timeout 200ms; true } catch { false }) { $finished = true; break }
    }
    if not $finished { try { job kill $job } }
    let text = (if ($out | path exists) { open --raw $out } else { "" })
    let connected = (openssl_address $text)
    let address = if $connected != null {
        { class: (address_class $connected), source: "the address openssl connected to" }
    } else if ($seen | is-not-empty) {
        { class: (address_class ($seen | first)), source: "the address a connection was attempted to, which did not complete" }
    } else {
        { class: "no connection", source: (if $finished { "openssl made none" } else { $"nothing within ($CONNECT_LIMIT)" }) }
    }
    { address: $address, verify: (openssl_verify $text) }
}

# One request, as the agent makes it: the URL as it is, with the path appended.
def ask [url: string, path: string, authorization: string] {
    let r = (try {
        { response: (http get --full --allow-errors --max-time $REQUEST_LIMIT -H [Accept application/json Authorization $authorization] $"($url)($path)"), error: null }
    } catch {|e|
        { response: null, error: { msg: ($e.msg? | default ""), debug: ($e.debug? | default "") } }
    })
    if $r.error != null {
        return { path: $path, status: null, error: (error_kind $r.error.msg $r.error.debug), detail: (error_detail $r.error.debug), anonymous: null, projects: null, deployment_type: null }
    }
    endpoint_facts $path $r.response
}

# The check, in the pod: the settings from the environment, the facts, what they mean; every line masked.
export def probe [] {
    let url = ($env.JIRA_URL? | default "" | str trim)
    let email = ($env.JIRA_EMAIL? | default "")
    let token = ($env.JIRA_TOKEN? | default "")
    let auth = (auth_mode $email)
    let credential = if $auth == "Basic" { $"($email):($token)" | encode base64 } else { "" }
    let authorization = if $auth == "Basic" { $"Basic ($credential)" } else { $"Bearer ($token)" }
    let host = (url_host $url)
    let connection = if $host == null {
        { address: { class: "no connection", source: "JIRA_URL has no host" }, verify: null }
    } else if (which openssl | is-empty) {
        { address: { class: "unknown", source: "the image has no openssl to connect with" }, verify: null }
    } else {
        connect_facts $host (url_port $url)
    }
    mut endpoints = []
    for p in $ENDPOINTS {
        let unreachable = ($endpoints | any {|e| $e.error != null })
        $endpoints = ($endpoints | append (if $host == null or $unreachable {
            { path: $p, status: null, error: "not tried", detail: null, anonymous: null, projects: null, deployment_type: null }
        } else {
            ask $url $p $authorization
        }))
    }
    let facts = {
        nu: (version | get version)
        url: (url_facts $url)
        deployment: (deployment_fact ($env.JIRA_DEPLOYMENT? | default ""))
        auth: $auth
        email_placeholder: (is_example_host ($email | split row "@" | last))
        token: (token_facts $token)
        address: $connection.address
        tls_verify: $connection.verify
        endpoints: $endpoints
    }
    let secrets = { url: $url, host: ($host | default ""), email: $email, token: $token, credential: $credential }
    for l in (printed $facts $secrets) { print $l }
}

# ── The pod (pure) ───────────────────────────────────────────────────────────

# The pod that runs the check: jira-observer's settings as values, its token from its Secret, nothing on a command line
# (the image's entrypoint runs /scripts/init.nu). `placement` is jira-observer's pod's nodeSelector, tolerations and
# imagePullSecrets, those it has.
export def check_pod [name: string, image: string, settings: record, token: record, placement: record] {
    {
        apiVersion: "v1"
        kind: "Pod"
        metadata: { name: $name, namespace: $POLAR, labels: { "polar.dev/scratch": $SCRATCH_VALUE } }
        spec: ({
            restartPolicy: "Never"
            automountServiceAccountToken: false
            enableServiceLinks: false
            activeDeadlineSeconds: $POD_DEADLINE_SECS
            securityContext: { runAsNonRoot: true, runAsUser: $POD_UID, runAsGroup: $POD_UID, seccompProfile: { type: "RuntimeDefault" } }
            containers: [{
                name: "check"
                image: $image
                imagePullPolicy: "IfNotPresent"
                env: [
                    { name: "HOME", value: "/tmp" }
                    { name: "XDG_CONFIG_HOME", value: "/tmp" }
                    { name: "JIRA_URL", value: $settings.url }
                    { name: "JIRA_EMAIL", value: $settings.email }
                    { name: "JIRA_DEPLOYMENT", value: $settings.deployment }
                    { name: "JIRA_TOKEN", valueFrom: { secretKeyRef: { name: $token.name, key: $token.key } } }
                ]
                securityContext: { allowPrivilegeEscalation: false, readOnlyRootFilesystem: true, capabilities: { drop: ["ALL"] } }
                volumeMounts: [{ name: "scripts", mountPath: "/scripts", readOnly: true } { name: "tmp", mountPath: "/tmp" }]
            }]
            volumes: [{ name: "scripts", configMap: { name: $name } } { name: "tmp", emptyDir: {} }]
        } | merge $placement)
    }
}

# The ConfigMap the pod's scripts come from: this file, and the init.nu the image runs, which calls `probe`.
export def scripts_config_map [name: string, script: string] {
    {
        apiVersion: "v1"
        kind: "ConfigMap"
        metadata: { name: $name, namespace: $POLAR, labels: { "polar.dev/scratch": $SCRATCH_VALUE } }
        data: { "init.nu": "use /scripts/jira-check.nu probe\nprobe\n", "jira-check.nu": $script }
    }
}

# The nu-init image a pod in `items` (Deployments, StatefulSets) runs, by the reference it runs it under, or null.
export def nu_image_of [items: list] {
    $items
    | each {|i| [...($i.spec?.template?.spec?.initContainers? | default []) ...($i.spec?.template?.spec?.containers? | default [])] }
    | flatten
    | get -o image
    | where {|r| $r != null and ($r =~ ('(^|/)' + $NU_IMAGE + '[:@]')) }
    | get -o 0
}

# ── The cluster ──────────────────────────────────────────────────────────────

def kube [...args: string] {
    let r = (^kubectl ...$args | complete)
    if $r.exit_code != 0 { error make --unspanned { msg: $"kubectl ($args | first 3 | str join ' ') failed: ($r.stderr | str trim)" } }
    $r.stdout
}

def apply [object: record] {
    let r = ($object | to json | ^kubectl apply -f - -o json | complete)
    if $r.exit_code != 0 { error make --unspanned { msg: $"the cluster refused the check's ($object.kind): ($r.stderr | str trim)" } }
    $r.stdout | from json
}

# Deletes the check pods an interrupted run left behind, once they have ended (by their deadline).
def delete_ended_checks [] {
    let r = (^kubectl -n $POLAR get pods -l $"($SCRATCH_LABEL)=($SCRATCH_VALUE)" -o json | complete)
    if $r.exit_code != 0 { return }
    for p in ($r.stdout | from json | get items | where {|p| ($p.status?.phase? | default "") in ["Succeeded" "Failed"] }) {
        ^kubectl -n $POLAR delete pod $p.metadata.name --ignore-not-found --wait=false | complete | ignore
    }
}

# Waits for the pod to end and returns its log. A pod that cannot start is an error that says why.
def wait_for [name: string] {
    let deadline = ((date now) + $WAIT_LIMIT)
    loop {
        let pod = (kube "-n" $POLAR "get" "pod" $name "-o" "json" | from json)
        let phase = ($pod.status?.phase? | default "")
        if $phase in ["Succeeded" "Failed"] { break }
        let waiting = ($pod.status?.containerStatuses? | default [] | get -o 0.state.waiting)
        if ($waiting.reason? | default "") in ["ErrImagePull" "ImagePullBackOff" "InvalidImageName" "CreateContainerConfigError" "CreateContainerError"] {
            error make --unspanned { msg: $"the check pod cannot start: ($waiting.reason)" }
        }
        if (date now) > $deadline { error make --unspanned { msg: $"the check pod did not end within ($WAIT_LIMIT)" } }
        sleep 2sec
    }
    kube "-n" $POLAR "logs" $name "-c" "check"
}

def run_check [name: string, image: string, settings: record, token: record, placement: record] {
    let map = (apply (scripts_config_map $name (open --raw $SELF)))
    let pod = (apply (check_pod $name $image $settings $token $placement))
    # The ConfigMap goes with the pod.
    let owner = { metadata: { ownerReferences: [{ apiVersion: "v1", kind: "Pod", name: $name, uid: $pod.metadata.uid }] } }
    kube "-n" $POLAR "patch" "configmap" $map.metadata.name "--type" "merge" "-p" ($owner | to json --raw) | ignore
    wait_for $name
}

def main [target: string] {
    if ($env.KUBECONFIG? | default "" | is-empty) {
        error make --unspanned { msg: "KUBECONFIG is not set (point it at the target's cluster)" }
    }
    if not ($INFRA | path join "targets" $target | path exists) {
        error make --unspanned { msg: $"no target '($target)' \(infra/targets/($target)\)" }
    }
    let document = (kube "-n" $POLAR "get" "configmap" $CONFIG_MAP "-o" "json" | from json | get data | get "config.json" | from json)
    let settings = (settings_of_document $document)
    let deployment = (kube "-n" $POLAR "get" "deployment" $WORKLOAD "-o" "json" | from json)
    let pod_spec = $deployment.spec.template.spec
    let token = ($pod_spec.containers | where name == $WORKLOAD | get -o 0.env | default [] | where name == "JIRA_TOKEN" | get -o 0.valueFrom.secretKeyRef | default $TOKEN_SECRET)
    # Only the Secret's key names are read here; its value goes to the pod alone.
    let keys = (kube "-n" $POLAR "get" "secret" $token.name "-o" "go-template={{range $k, $v := .data}}{{$k}} {{end}}" | split row " " | where {|k| $k | is-not-empty })
    if $token.key not-in $keys { error make --unspanned { msg: $"the Secret ($token.name) has no key ($token.key), which jira-observer's token comes from" } }
    let image = (nu_image_of (kube "get" "deployments,statefulsets" "-A" "-o" "json" | from json | get items))
    if $image == null { error make --unspanned { msg: $"nothing in the cluster runs ($NU_IMAGE), the image the check runs in" } }
    let placement = (["nodeSelector" "tolerations" "imagePullSecrets"] | reduce --fold {} {|k, acc|
        let v = ($pod_spec | get -o $k)
        if $v == null { $acc } else { $acc | insert $k $v }
    })
    let declared = ((open ($INFRA | path join "targets" $target "config.nuon")).workloads?."jira-observer"?.env? | default [] | where name == "JIRA_URL" | get -o 0.value)
    let secrets = { url: $settings.url, host: (url_host $settings.url | default ""), email: $settings.email }

    delete_ended_checks
    let name = $"jira-check-(random chars --length 6 | str lowercase)"
    print $"jira-observer's Jira connection, checked from a pod on its node pool \(($target)\)"
    print $"  the cluster's JIRA_URL is the target's: (if $declared == null { 'the target does not set it' } else { yes_no ($declared == $settings.url) })"
    let result = (try { { log: (run_check $name $image $settings $token $placement), error: null } } catch {|e| { log: "", error: $e.msg } })
    ^kubectl -n $POLAR delete pod $name --ignore-not-found --wait=false | complete | ignore
    ^kubectl -n $POLAR delete configmap $name --ignore-not-found --wait=false | complete | ignore
    for l in ($result.log | lines | where {|l| not ($l | str starts-with "[nu-init]") }) { print (mask $l $secrets) }
    if $result.error != null {
        print -e (mask $"the check did not finish: ($result.error)" $secrets)
        exit 1
    }
}
