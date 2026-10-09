#!/usr/bin/env nu
# infra/tests/rules.nu
#
# Tests of the rules in infra/rules.nu: the checks `resolve.nu check` runs over a
# workload's health and shutdown settings. The rules are pure functions of a record,
# so each case here is a record and the problems it should produce.
#
#   nu infra/tests/rules.nu
#
# The health rules are not here: they are Rust, in the agent, and the resolver asks the agent
# itself (`<agent> --check --config FILE`, see tests/resolve.nu). What is here is what only the
# deployment knows: the grace period against the budget, how Jobs replace pods, the apply order.

use std/assert
use ../rules.nu *

const GOOD_SHUTDOWN = { budget_secs: 20, grace_margin_secs: 10, termination_grace_secs: 30 }

def has [problems: list<string>, needle: string] {
    $problems | any {|p| $p | str contains $needle }
}

def main [] {
    let tests = {
        "a Job that replaces pods only after they are gone passes": {
            let docs = [{ kind: "Job", metadata: { name: "kube-consumer" }, spec: { podReplacementPolicy: "Failed" } }]
            assert equal (job_replacement_problems $docs) []
        }

        "a Job with the default policy is a problem, named": {
            let docs = [{ kind: "Job", metadata: { name: "kube-consumer" }, spec: {} }]
            let p = (job_replacement_problems $docs)
            assert equal ($p | length) 1
            assert (has $p "kube-consumer")
            assert (has $p "podReplacementPolicy")
        }

        "the other policy is a problem too": {
            let docs = [{ kind: "Job", metadata: { name: "jira-processor" }, spec: { podReplacementPolicy: "TerminatingOrFailed" } }]
            assert equal (job_replacement_problems $docs | length) 1
        }

        "things that are not Jobs are not asked for the policy": {
            let docs = [
                { kind: "Deployment", metadata: { name: "cert-issuer" }, spec: {} }
                { kind: "ServiceAccount", metadata: { name: "x" } }
            ]
            assert equal (job_replacement_problems $docs) []
        }

        "the default shutdown settings have no problems": {
            assert equal (shutdown_problems "w" $GOOD_SHUTDOWN) []
        }

        "a budget under a second is a problem": {
            let p = (shutdown_problems "w" ($GOOD_SHUTDOWN | update budget_secs 0 | update termination_grace_secs 10))
            assert (has $p "shutdown.budget_secs must be a whole number of seconds, at least 1")
        }

        "the grace period must exceed the budget": {
            let p = (shutdown_problems "w" ($GOOD_SHUTDOWN | update termination_grace_secs 20))
            assert (has $p "must exceed the shutdown budget")
            assert equal (shutdown_problems "w" ($GOOD_SHUTDOWN | update termination_grace_secs 21)) []
        }

        "a rendered manifest that the apply order does not list is a problem naming it": {
            let p = (apply_order_problems ["a.yaml" "a-resources.yaml" "b.yaml"] ["a.yaml" "b.yaml"])
            assert equal ($p | length) 1
            assert (has $p "a-resources.yaml")
        }

        "a manifest in the apply order is not a problem, and an order that lists more is fine": {
            assert equal (apply_order_problems ["a.yaml"] ["x.yaml" "a.yaml" "y.yaml"]) []
        }

        "a resources manifest must be applied before the workload that mounts it": {
            let p = (apply_order_problems ["a.yaml" "a-resources.yaml"] ["a.yaml" "a-resources.yaml"])
            assert equal ($p | length) 1
            assert (has $p "before")
        }

        "the margin must be at least a second": {
            let p = (shutdown_problems "w" ($GOOD_SHUTDOWN | update grace_margin_secs 0 | update termination_grace_secs 20))
            assert (has $p "shutdown.grace_margin_secs must be at least 1")
        }
    }

    let results = ($tests | transpose name body | each {|t|
        try {
            do $t.body
            { name: $t.name, error: null }
        } catch {|e|
            { name: $t.name, error: $e.msg }
        }
    })

    for r in $results {
        if $r.error == null {
            print $"  ok    ($r.name)"
        } else {
            print -e $"  FAIL  ($r.name): ($r.error)"
        }
    }

    let failed = ($results | where error != null | length)
    if $failed > 0 {
        print -e $"($failed) of ($results | length) failed"
        exit 1
    }
    print $"all ($results | length) passed"
}
