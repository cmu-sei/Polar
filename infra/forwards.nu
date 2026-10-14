#!/usr/bin/env nu
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

