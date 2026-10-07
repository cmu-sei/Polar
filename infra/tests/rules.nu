#!/usr/bin/env nu
# infra/tests/rules.nu
#
# Tests of the rules in infra/rules.nu: the checks `resolve.nu check` runs over a
# workload's health and shutdown settings. The rules are pure functions of a record,
# so each case here is a record and the problems it should produce.
#
#   nu infra/tests/rules.nu
#
# The health rules are not here: they are Rust, and `environment_problems` runs the very
# function the agent runs when it starts, through polar-config-check. What is here is what
# only the deployment knows: the grace period against the budget.

use std/assert
use ../rules.nu *

const GOOD_SHUTDOWN = { budget_secs: 20, grace_margin_secs: 10, termination_grace_secs: 30 }

def has [problems: list<string>, needle: string] {
    $problems | any {|p| $p | str contains $needle }
}

def main [] {
    let tests = {
        "a workload's real environment passes the real checker": {
            let entries = [
                { name: "POLAR_CAPABILITIES", value: "cassini,graph-write" }
                { name: "POLAR_SHUTDOWN_BUDGET_SECS", value: "45" }
                { name: "POLAR_HEALTH_TICK_SECS", value: "30" }
                { name: "POLAR_HEALTH_REJUVENATION_SECS", value: "300" }
            ]
            assert equal (environment_problems "w" $entries) []
        }

        "a bad environment fails, and each problem names the workload": {
            let entries = [
                { name: "POLAR_SHUTDOWN_BUDGET_SECS", value: "0" }
                { name: "POLAR_HEALTH_TICK_SECS", value: "soon" }
            ]
            let p = (environment_problems "kube-consumer" $entries)
            assert equal ($p | length) 2
            assert ($p | all {|x| $x | str starts-with "kube-consumer: " })
            assert (has $p "POLAR_SHUTDOWN_BUDGET_SECS")
            assert (has $p "POLAR_HEALTH_TICK_SECS")
        }

        "a relation between variables is caught, with the variables named": {
            let p = (environment_problems "w" [{ name: "POLAR_HEALTH_REJUVENATION_SECS", value: "250" }])
            assert equal ($p | length) 1
            assert (has $p "POLAR_HEALTH_REJUVENATION_SECS")
            assert (has $p "hard expiry floor")
        }

        "entries that come from a secret are skipped, not passed as empty": {
            let entries = [
                { name: "GRAPH_PASSWORD", value: null, secret: "polar-graph-pw", key: "secret" }
                { name: "POLAR_HEALTH_TICK_SECS", value: "30" }
            ]
            assert equal (environment_problems "w" $entries) []
        }

        "the caller's own environment does not leak into the check": {
            # The shell running the tests may have these set; the check must not see them.
            with-env { POLAR_HEALTH_TICK_SECS: "soon", POLAR_SHUTDOWN_BUDGET_SECS: "0" } {
                assert equal (environment_problems "w" []) []
            }
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
