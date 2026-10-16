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

# Runs `probe` (url -> { skipped, exit_code, stderr }) and says what it found. Returns { verdict, lines }.
export def egress_report [url: string, probe: closure] {
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
