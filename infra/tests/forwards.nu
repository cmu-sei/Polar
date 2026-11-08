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

# The port-forwards up.nu, deploy-repo.nu and cluster-registry.nu rely on are this cluster's, not another's left running
# (docs/architecture/key-custody.md, step 7, finding 1).

use std/assert
use ../forwards.nu *
use ../retry.nu [with_retries]

def main [] {
    let tests = [
        {
            name: "a forward under another kubeconfig is stale, and one under this cluster's is live"
            body: {
                let found = [
                    { pid: 10, kubeconfig: "/clusters/u7s" }
                    { pid: 11, kubeconfig: "/clusters/u7c" }
                    { pid: 12, kubeconfig: "" }
                ]
                assert equal (stale $found "/clusters/u7c") [10 12]
                assert (live $found "/clusters/u7c")
                assert (not (live ($found | where pid != 11) "/clusters/u7c"))
            }
        }
        {
            name: "with no kubeconfig set, a forward with none is this cluster's"
            body: {
                assert equal (stale [{ pid: 12, kubeconfig: "" }] "") []
                assert (live [{ pid: 12, kubeconfig: "" }] "")
                assert (not (live [] "/clusters/u7c"))
            }
        }
        {
            # macOS has no /proc: which cluster a forward reaches cannot be read there. It is left running and counted as
            # running; whether it answers is checked where it is used (the registry's and the observer's are restarted
            # when they do not).
            name: "a forward whose environment cannot be read is not stopped, and counts as running"
            body: {
                let found = [{ pid: 13, kubeconfig: null }]
                assert equal (stale $found "/clusters/u7c") []
                assert (live $found "/clusters/u7c")
                assert (live $found "")
            }
        }
        {
            name: "a process's KUBECONFIG is read from its environment: none set is \"\", none readable is null"
            body: {
                assert equal (kubeconfig_in $"PATH=/bin(char nul)KUBECONFIG=/clusters/u7c(char nul)HOME=/h(char nul)") "/clusters/u7c"
                assert equal (kubeconfig_in $"PATH=/bin(char nul)HOME=/h(char nul)") ""
                assert equal (kubeconfig_in null) null
            }
        }
        {
            # macOS has no setsid: the forward's loop, started in the background, failed there and the recipe still said it
            # had started it, so up.nu's step 2 waited for a registry nothing forwarded to.
            name: "just port-forwards-up starts its forward where there is no setsid"
            body: {
                let root = ($env.FILE_PWD | path dirname | path dirname)
                let just = (which just | get 0.path)
                # A PATH with only what the recipe and the Justfile's own variables run, and a kubectl that says who ran it.
                let bin = (mktemp --directory)
                for tool in [bash sh nohup sleep uname] { ^ln -s (which -a $tool | where type == external | get 0.path) ($bin | path join $tool) }
                let seen = ($bin | path join "seen")
                $"#!/bin/sh\necho \"$PPID $$\" > ($seen)\nexec sleep 30\n" | save ($bin | path join "kubectl")
                ^chmod +x ($bin | path join "kubectl")
                let r = (with-env { PATH: [$bin] } { do { cd $root; ^$just port-forwards-up jaeger } | complete })
                let started = (with_retries 20 250ms {|n| { ok: ($seen | path exists), final: false, detail: "no kubectl was run" } })
                # The loop and its kubectl, by the pids the kubectl wrote: nothing else is stopped.
                if ($seen | path exists) { ^kill ...(open --raw $seen | str trim | split row " ") }
                rm -rf $bin
                assert equal $r.exit_code 0 $r.stderr
                assert $started.ok "the forward's loop never ran kubectl"
            }
        }
        {
            name: "a real loop left under another kubeconfig is found, stopped by its pid, and nothing else is"
            body: {
                # `just port-forwards-up`'s loop, its command line exactly, with a kubectl that only waits.
                let bin = (mktemp --directory)
                "#!/bin/sh\nexec sleep 30\n" | save ($bin | path join "kubectl")
                ^chmod +x ($bin | path join "kubectl")
                let loop = "while true; do kubectl port-forward -n polar-forwards-test svc/forwards-test 1:1 > /dev/null 2>&1; sleep 1; done"
                let job = (job spawn {
                    with-env { KUBECONFIG: "/clusters/gone", PATH: ($env.PATH | prepend $bin) } {
                        do { ^bash -c $loop } | complete | ignore
                    }
                })
                sleep 500ms
                let found = (processes "polar-forwards-test" "forwards-test")
                assert equal ($found | length) 1 $"the loop alone: ($found)"
                assert equal $found.0.kubeconfig "/clusters/gone"
                assert (not (forward_live "polar-forwards-test" "forwards-test" "/clusters/here"))
                assert (forward_live "polar-forwards-test" "forwards-test" "/clusters/gone")
                assert equal (stop_stale "polar-forwards-test" "forwards-test" "/clusters/here") 1
                sleep 300ms
                assert equal (processes "polar-forwards-test" "forwards-test") []
                try { job kill $job }
                rm -rf $bin
            }
        }
    ]

    let results = ($tests | each {|t|
        try { do $t.body; { name: $t.name, error: null } } catch {|e| { name: $t.name, error: $e.msg } }
    })
    for r in $results {
        if $r.error == null { print $"  ok    ($r.name)" } else { print -e $"  FAIL  ($r.name): ($r.error)" }
    }
    let failed = ($results | where error != null | length)
    if $failed > 0 { print -e $"($failed) of ($results | length) failed"; exit 1 }
    print $"all ($results | length) passed"
}
