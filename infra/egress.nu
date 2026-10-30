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

# infra/egress.nu
#
# Whether a pod can reach a host outside the cluster. The git agents need it to read a repository that is not in the
# cluster, and it fails quietly when the cluster's DNS has nowhere to send a name it does not own: git says "Could
# not resolve host" long after the bring-up that caused it. up.nu runs this at the end and says what it finds. It
# reports and never stops the bring-up: a cluster used offline is a working cluster.
#
# The probe is `git ls-remote` run in the polar-scheduler-observer pod, the one image that has a git CLI. It reads
# one public repository and sends no credentials.
#
# And the target's way out (docs/architecture/egress.md): `cluster.egress` in its config.nuon, a proxy, the hosts that
# bypass it, and a CA to trust beside the system's, all optional. `settings_of` judges it; `egress_entries` is what a
# workload whose agent declares the egress section (polar::egress) is given for it. A target that sets nothing gives
# nothing, and every pod is as it was.

const POLAR = "polar"
const PROBE_JOB = "polar-scheduler-observer"
const DEFAULT_URL = "https://github.com/octocat/Hello-World"

# How long the probe may take. The pod has no `timeout`, so the limit is on kubectl.
const PROBE_SECS = 30

# The URL probed: POLAR_EGRESS_PROBE_URL if set.
export def probe_url [] { $env.POLAR_EGRESS_PROBE_URL? | default $DEFAULT_URL }

# What git's answer means. `ok` includes an HTTP error from the server: the network got there. A probe killed for
# taking too long (exit 124, from `timeout`) printed nothing; it hung, as a connection to a black hole does.
# The wording is git's, as observed from the pod: "Could not resolve host", "Failed to connect to", "The requested
# URL returned error".
export def egress_verdict [exit_code: int, stderr: string] {
    if $exit_code == 0 { return "ok" }
    if $exit_code == 124 { return "unreachable" }
    if ($stderr | str contains "Could not resolve host") { return "dns" }
    if ($stderr | str contains "The requested URL returned error") { return "ok" }
    if ($stderr | str contains "Failed to connect to") { return "unreachable" }
    "other"
}

# The lines to print for a verdict. Every one is a note: none of them stops anything.
export def egress_lines [verdict: string, url: string, stderr: string] {
    match $verdict {
        "ok" => [$"pods can reach ($url)"]
        "dns" => [
            $"pods cannot resolve names outside the cluster \(git could not resolve the host in ($url)\). Anything that reads a repository outside the cluster, such as the git agents, will fail until they can. If you are working offline, ignore this."
            "CoreDNS sends names it does not own to the upstream in its Corefile. Its errors: kubectl -n kube-system logs -l k8s-app=kube-dns --tail=20"
            "If the cluster's DNS forwards to a resolver that runs only on the host (a second systemd-resolved listener, say), is that listener there: ss -lun 'sport = :5300' (the port is whatever the cluster forwards to)"
        ]
        "unreachable" => [
            $"pods resolved the name but could not connect to ($url), or the attempt hung. Check the host's network, VPN and firewall. If you are working offline, ignore this."
        ]
        _ => [
            $"the outside-access probe of ($url) failed in a way this check does not recognise: ($stderr | lines | where {|l| $l | is-not-empty } | first | default '')"
        ]
    }
}

# Runs `probe` (url -> { skipped, exit_code, stderr }) and says what it found. Returns { verdict, lines }. A target that
# goes out through a proxy (`proxy`, its cluster.egress.proxy) is not probed: git in the pod knows nothing of it.
export def egress_report [url: string, probe: closure, proxy: any = null] {
    if $proxy != null {
        return { verdict: "skipped", lines: [$"skipped: the target reaches the outside through cluster.egress.proxy, which the probe \(git in ($PROBE_JOB)\) does not use"] }
    }
    let r = (do $probe $url)
    if $r.skipped {
        return { verdict: "skipped", lines: [$"no ($PROBE_JOB) pod is running to probe from, so access to the outside was not checked"] }
    }
    let verdict = (egress_verdict $r.exit_code $r.stderr)
    { verdict: $verdict, lines: (egress_lines $verdict $url $r.stderr) }
}

# The probe against the cluster: `git ls-remote <url> HEAD` in the running polar-scheduler-observer pod.
export def kube_probe [url: string] {
    let pods = (^kubectl -n $POLAR get pods -l $"job-name=($PROBE_JOB)" --field-selector status.phase=Running -o name | complete)
    let pod = if $pods.exit_code == 0 { $pods.stdout | lines | where {|l| $l | is-not-empty } | get -o 0 } else { null }
    if $pod == null { return { skipped: true, exit_code: 0, stderr: "" } }
    let r = (^timeout $PROBE_SECS kubectl -n $POLAR exec -c $PROBE_JOB $pod -- git ls-remote $url HEAD | complete)
    { skipped: false, exit_code: $r.exit_code, stderr: $r.stderr }
}

# ── The target's egress setting (pure, but for reading the CA file) ──────────

const EGRESS_KEYS = ["proxy" "no_proxy" "ca_file"]

# The most a ConfigMap holds: Kubernetes refuses one whose data is over 1 MiB.
const CONFIG_MAP_BYTES = 1048576

# Where a pod finds the egress CA: a ConfigMap of the workload's own, mounted read-only.
const CA_VOLUME = "polar-egress-ca"
const CA_DIR = "/etc/polar-egress-ca"
const CA_KEY = "ca.pem"

# The variable whose declaration says an agent takes the egress section (polar::egress::SECTION).
const SENTINEL = "POLAR_EGRESS_PROXY"

const PROXY_SHAPE = "a proxy is an http:// or https:// URL with a host and a port, such as http://proxy.example.org:3128"
const ENTRY_SHAPE = "is not a host, .domain suffix, IP address or CIDR (no commas, spaces or empty entries)"

def refuse [msg: string] { error make --unspanned { msg: $msg } }

# A value as a message shows it: text as it is, anything else as nuon.
def shown [value: any] { if ($value | describe) == "string" { $value } else { $value | to nuon } }

# What reaches the proxy is never the cluster's own: loopback, every Service (`.svc`, and the cluster's domain), then
# what the target adds, each once, in that order. Written only when there is a proxy.
export def no_proxy_of [domain: string, extra: list] {
    ["localhost" "127.0.0.1" "::1" ".svc" $".($domain)"] ++ $extra | uniq
}

# An http or https URL with a host and a port, nothing after it but a slash, and no user or password: the value is
# written into a ConfigMap, which is no place for a credential. A refusal never repeats what may be one.
def check_proxy [proxy: any] {
    let shape = $"cluster.egress.proxy is '(shown $proxy)': ($PROXY_SHAPE)"
    if ($proxy | describe) != "string" { refuse $shape }
    if ($proxy =~ '^[A-Za-z][A-Za-z0-9+.-]*://[^/?#]*@') {
        refuse "cluster.egress.proxy names a user or password: it is written into a ConfigMap, and a proxy that needs credentials is not supported"
    }
    let m = ($proxy | parse --regex '^(?<scheme>https?)://(?<host>\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9](?:[A-Za-z0-9.-]*[A-Za-z0-9])?):(?<port>[0-9]{1,5})/?$')
    if ($m | is-empty) { refuse $shape }
    let port = ($m.0.port | into int)
    if $port < 1 or $port > 65535 { refuse $shape }
}

# One entry of the bypass list as hyper-util reads it: a host, a .domain suffix, an IPv4 or IPv6 address, or a CIDR.
def entry_ok [entry: string] {
    let label = '[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?'
    let host = ('^\.?' + $label + '(?:\.' + $label + ')*$')
    let v4 = '^[0-9]{1,3}(?:\.[0-9]{1,3}){3}(?:/[0-9]{1,2})?$'
    let v6 = '^[0-9A-Fa-f]*:[0-9A-Fa-f:.]*(?:/[0-9]{1,3})?$'
    ($entry =~ $host) or ($entry =~ $v4) or ($entry =~ $v6)
}

def check_no_proxy [extra: any] {
    let kind = ($extra | describe)
    if not ($kind | str starts-with "list") {
        refuse $"cluster.egress.no_proxy is ($kind): it is a list of hosts, .domain suffixes, IP addresses and CIDRs"
    }
    for e in $extra {
        if $e == "*" {
            refuse "cluster.egress.no_proxy has '*', which sends nothing through the proxy: remove cluster.egress.proxy instead"
        }
        if ($e | describe) != "string" or not (entry_ok $e) {
            refuse $"cluster.egress.no_proxy entry '(shown $e)' ($ENTRY_SHAPE)"
        }
    }
}

# The CA file's text: a file of the target's own directory, small enough for a ConfigMap, of PEM certificates only.
def check_ca [ca_file: any, target_dir: string] {
    let inside = $"infra/targets/($target_dir | path basename)"
    let named = (shown $ca_file)
    if ($ca_file | describe) != "string" or ($ca_file | is-empty) or ($ca_file | str starts-with "/") or (".." in ($ca_file | path split)) {
        refuse $"cluster.egress.ca_file is '($named)': it is a path inside ($inside)"
    }
    let path = ($target_dir | path join $ca_file)
    if ($path | path type) != "file" {
        refuse $"cluster.egress.ca_file ($ca_file) does not exist \(it is read relative to ($inside)\)"
    }
    let size = (ls $path | get 0.size | into int)
    if $size > $CONFIG_MAP_BYTES {
        refuse $"cluster.egress.ca_file ($ca_file) is ($size) bytes, more than a ConfigMap holds"
    }
    let raw = (open --raw $path)
    let text = if ($raw | describe) == "string" { $raw } else { try { $raw | decode utf-8 } catch { "" } }
    if ($text =~ '-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----') {
        refuse $"cluster.egress.ca_file ($ca_file) holds a private key: it must hold only CA certificates, and it is published in a ConfigMap"
    }
    let blocks = ($text | parse --regex '-----BEGIN CERTIFICATE-----(?<body>[\s\S]*?)-----END CERTIFICATE-----')
    if ($blocks | is-empty) {
        refuse $"cluster.egress.ca_file ($ca_file) holds no PEM certificate \(no -----BEGIN CERTIFICATE----- block\)"
    }
    for b in $blocks {
        let body = ($b.body | str replace --all --regex '\s' '')
        let decodes = (try { $body | decode base64 | ignore; true } catch { false })
        if ($body | is-empty) or (not $decodes) {
            refuse $"cluster.egress.ca_file ($ca_file) holds a CERTIFICATE block that is not base64"
        }
    }
    $text
}

# The target's egress setting, from its merged cluster settings (`cluster.nuon` under the target's own) and its directory
# (infra/targets/<t>, which the CA file is read from): { proxy, no_proxy (with the defaults), ca_pem }, or null when it
# sets none (`egress` absent, empty, or every key null). Anything that cannot work is an error that says why.
export def settings_of [cluster: record, target_dir: string] {
    let e = ($cluster.egress? | default null)
    if $e == null { return null }
    let kind = ($e | describe)
    if not ($kind | str starts-with "record") {
        refuse $"cluster.egress is ($kind): it is a record with any of proxy, no_proxy, ca_file"
    }
    for k in ($e | columns) {
        if not ($k in $EGRESS_KEYS) {
            refuse $"cluster.egress.($k) is not a setting: the settings are proxy, no_proxy, ca_file"
        }
    }
    let proxy = ($e.proxy? | default null)
    let extra = ($e.no_proxy? | default null)
    let ca_file = ($e.ca_file? | default null)
    if $proxy == null and $extra == null and $ca_file == null { return null }
    if $proxy != null { check_proxy $proxy }
    if $extra != null {
        if $proxy == null {
            refuse "cluster.egress.no_proxy is set but cluster.egress.proxy is not: there is nothing to bypass"
        }
        check_no_proxy $extra
    }
    let ca_pem = if $ca_file == null { null } else { check_ca $ca_file $target_dir }
    { proxy: $proxy, no_proxy: (no_proxy_of $cluster.domain ($extra | default [])), ca_pem: $ca_pem }
}

def egress_variable [name: string, value: string] {
    { name: $name, value: $value, secret: null, key: null, from: "egress" }
}

# What `workload` is given for the egress setting `settings` (settings_of), when its agent declares the egress section
# (`declared`, the variables its scaffold declares, has POLAR_EGRESS_PROXY): the variables, written into its document,
# and for a CA the ConfigMap that holds it, a volume of it and its mount. Nothing, otherwise.
export def egress_entries [settings: any, workload: string, declared: list] {
    let none = { env: [], volumes: [], mounts: [], config_maps: [] }
    if $settings == null or not ($SENTINEL in $declared) { return $none }
    let proxy = if $settings.proxy == null { [] } else {
        [
            (egress_variable "POLAR_EGRESS_PROXY" $settings.proxy)
            (egress_variable "POLAR_EGRESS_NO_PROXY" ($settings.no_proxy | str join ","))
        ]
    }
    if $settings.ca_pem == null { return ($none | update env $proxy) }
    let config_map = $"($workload)-egress-ca"
    {
        env: ($proxy | append (egress_variable "POLAR_EGRESS_CA_CERT" $"($CA_DIR)/($CA_KEY)"))
        volumes: [{ name: $CA_VOLUME, secret: null, audience: null, config_map: $config_map, from: "egress" }]
        mounts: [{ name: $CA_VOLUME, path: $CA_DIR, read_only: true, from: "egress" }]
        config_maps: [{ name: $config_map, key: $CA_KEY, content: $settings.ca_pem }]
    }
}
