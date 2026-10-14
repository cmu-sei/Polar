#!/usr/bin/env nu
# infra/tests/egress.nu
#
# The check at the end of up.nu that a pod can reach an outside host: how git's answer is read, what is said for
# each, and that the check reports and never stops the bring-up (a person working offline has a working cluster).
# The messages are the ones git printed from inside the polar-scheduler-observer pod.
#
#   nu infra/tests/egress.nu

use std/assert
use ../egress.nu *

const DNS = "fatal: unable to access 'https://github.com/octocat/Hello-World/': Could not resolve host: github.com"
const REFUSED = "fatal: unable to access 'https://127.0.0.1:1/x/': Failed to connect to 127.0.0.1:1 after 0 ms: Could not connect to server"
const HTTP = "fatal: unable to access 'https://140.82.112.3/octocat/Hello-World/': The requested URL returned error: 301"

def has [lines: list, needle: string] { $lines | any {|l| $l | str contains $needle } }

def main [] {
    let tests = [
        {
            name: "success is reachable"
            body: { assert equal (egress_verdict 0 "") "ok" }
        }
        {
            name: "an HTTP error from the server means the network works"
            body: { assert equal (egress_verdict 128 $HTTP) "ok" }
        }
        {
            name: "a name that does not resolve is a DNS problem, not a network one"
            body: { assert equal (egress_verdict 128 $DNS) "dns" }
        }
        {
            name: "a refused or failed connection is unreachable"
            body: { assert equal (egress_verdict 128 $REFUSED) "unreachable" }
        }
        {
            name: "a probe that hung until it was killed (exit 124 from timeout, no output) is unreachable"
            body: { assert equal (egress_verdict 124 "") "unreachable" }
        }
        {
            name: "anything else is not guessed at"
            body: { assert equal (egress_verdict 128 "fatal: something nobody has seen") "other" }
        }
        {
            name: "the DNS advice says what is wrong, that offline can ignore it, and where to look"
            body: {
                let l = (egress_lines "dns" "https://example.org/r" $DNS)
                assert (has $l "cannot resolve names outside the cluster")
                assert (has $l "offline")
                assert (has $l "kubectl -n kube-system logs -l k8s-app=kube-dns")
                assert (has $l "5300")
            }
        }
        {
            name: "the unreachable advice does not blame DNS"
            body: {
                let l = (egress_lines "unreachable" "https://example.org/r" $REFUSED)
                assert (has $l "could not connect")
                assert (not (has $l "cannot resolve"))
            }
        }
        {
            name: "an answer nobody recognises is shown, not hidden"
            body: {
                let l = (egress_lines "other" "https://example.org/r" "fatal: something nobody has seen")
                assert (has $l "something nobody has seen")
            }
        }
        {
            name: "a pod that can be probed from and a good answer: one line saying so, ok"
            body: {
                let r = (egress_report "https://example.org/r" {|u| { skipped: false, exit_code: 0, stderr: "" } })
                assert equal $r.verdict "ok"
                assert equal ($r.lines | length) 1
                assert ($r.lines.0 | str contains "https://example.org/r")
            }
        }
        {
            name: "no pod to probe from is said, and is not a failure"
            body: {
                let r = (egress_report "https://example.org/r" {|u| { skipped: true, exit_code: 0, stderr: "" } })
                assert equal $r.verdict "skipped"
                assert (has $r.lines "was not checked")
            }
        }
        {
            name: "a DNS failure is reported with its advice"
            body: {
                let r = (egress_report "https://github.com/x" {|u| { skipped: false, exit_code: 128, stderr: $DNS } })
                assert equal $r.verdict "dns"
                assert (has $r.lines "cannot resolve")
            }
        }
        {
            name: "the probe is given the URL it is to try"
            body: {
                let r = (egress_report "https://example.org/the-url" {|u| { skipped: false, exit_code: 128, stderr: $"asked ($u)" } })
                assert (has $r.lines "asked https://example.org/the-url")
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
