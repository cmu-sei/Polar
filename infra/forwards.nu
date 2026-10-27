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

# infra/forwards.nu
#
# The port-forwards up.nu depends on to talk to the cluster from this machine. "Already running" is not
# "answering": a forward can outlive the pod it was made for, and every pod restarts hourly.

use retry.nu [with_retries]

# Where the observer's port-forward listens on this machine (`just port-forwards-up observer`, Justfile).
const OBSERVER_LOCAL_URL = "http://localhost:8081"

# Whether the observer forward answers: any HTTP reply is an answer, and nothing is not.
export def observer_answers [] {
    let r = (do { ^curl -s -m 4 -o /dev/null -w "%{http_code}" $"($OBSERVER_LOCAL_URL)/" } | complete)
    ($r.exit_code == 0) and ($r.stdout != "000")
}

# Stops a port-forward and starts it again, loop first or the loop would restart what is stopped.
export def restart_forward [svc: string, name: string] {
    do { ^pkill -f $"while true; do kubectl port-forward -n polar svc/($svc)" } | complete | ignore
    do { ^pkill -f $"kubectl port-forward -n polar svc/($svc)" } | complete | ignore
    ^just port-forwards-up $name
    if $env.LAST_EXIT_CODE != 0 { error make { msg: $"just port-forwards-up ($name) failed" } }
}

# The admin tool talks to the observer through its forward. "Already running" is not "answering": a forward
# can outlive the pod it was made for. Wait for it to answer, restart it once if it does not, and say so if
# it still does not.
export def ensure_observer_forward [] {
    let wait = {|attempts| with_retries $attempts 3sec {|n| { ok: (observer_answers), final: false, detail: $"nothing answers at ($OBSERVER_LOCAL_URL)" } } }
    if (do $wait 5).ok { return }
    print "  -- the observer's port-forward is running but does not answer; restarting it"
    restart_forward "credentials-agent-observer" "observer"
    if not (do $wait 10).ok {
        error make { msg: $"the credentials agent's observer does not answer at ($OBSERVER_LOCAL_URL) after restarting its port-forward: is the pod running? \(kubectl -n polar get pods -l job-name=credentials-agent-observer\)" }
    }
    print "  ok: the observer's port-forward answers again"
}

# ── Which cluster a forward reaches ───────────────────────────────────────────────────────────────────────────
#
# The port-forwards `just port-forwards-up` starts (a `bash -c "while true; do kubectl port-forward -n <ns> svc/<svc>
# ..."` loop, detached, and its kubectl), told apart by the cluster they reach. A loop keeps the KUBECONFIG it was
# started with, so one left from another cluster's run (u7s, then u7c) holds the port and reaches a cluster that is not
# there: counting it as running made a service look down (docs/architecture/key-custody.md, step 7, finding 1). A
# forward is live only when it runs under this shell's KUBECONFIG, and the others are stopped, by their pids.

# The processes of the forward to `svc` in `ns` (the loop and its kubectl), each with the KUBECONFIG it runs under ("" when
# none, or when it cannot be read).
export def processes [ns: string, svc: string]: nothing -> list<record<pid: int, kubeconfig: string>> {
    # The loop's command line or its kubectl's, from its start (either by a path or by name): not every process that
    # mentions the service (a shell running a command that names it, say).
    let pattern = $"^\([^ ]*bash -c while true; do \)?\([^ ]*/\)?kubectl port-forward -n ($ns) svc/($svc) "
    let found = (do { ^pgrep -f $pattern } | complete)
    if $found.exit_code != 0 { return [] }
    $found.stdout | lines | where {|l| $l | str trim | is-not-empty } | each {|p|
        let pid = ($p | into int)
        let kubeconfig = (try {
            open --raw $"/proc/($pid)/environ" | split row (char nul)
            | where {|v| $v | str starts-with "KUBECONFIG=" }
            | each {|v| $v | str replace "KUBECONFIG=" "" } | first
        } catch { "" })
        { pid: $pid, kubeconfig: ($kubeconfig | default "") }
    }
}

# The pids among `found` that reach another cluster than the one `current` names.
export def stale [found: list, current: string]: nothing -> list<int> {
    $found | where kubeconfig != $current | get pid
}

# Whether any of `found` reaches the cluster `current` names.
export def live [found: list, current: string]: nothing -> bool {
    $found | any {|p| $p.kubeconfig == $current }
}

# Whether the forward to `svc` in `ns` runs for the cluster `current` names (this shell's KUBECONFIG by default).
export def forward_live [ns: string, svc: string, current?: string]: nothing -> bool {
    live (processes $ns $svc) ($current | default ($env.KUBECONFIG? | default ""))
}

# Stops the forward's processes that reach another cluster, by their pids, and says how many.
export def stop_stale [ns: string, svc: string, current?: string]: nothing -> int {
    let pids = (stale (processes $ns $svc) ($current | default ($env.KUBECONFIG? | default "")))
    for pid in $pids { do { ^kill $pid } | complete | ignore }
    $pids | length
}
