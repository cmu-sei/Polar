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

# What a workload with the cassini and graph-write capabilities is given.
def sound_entries []: nothing -> list {
    [
        { name: "POLAR_CAPABILITIES", value: "cassini,graph-write" }
        { name: "TLS_CLIENT_CERT", value: "/etc/tls/certs/cert.pem" }
        { name: "TLS_CLIENT_KEY", value: "/etc/tls/certs/key.pem" }
        { name: "TLS_CA_CERT", value: "/etc/tls/certs/ca.pem" }
        { name: "BROKER_ADDR", value: "cassini:8080" }
        { name: "CASSINI_SERVER_NAME", value: "cassini.local" }
        { name: "GRAPH_DB", value: "neo4j" }
        { name: "GRAPH_USER", value: "neo4j" }
        { name: "GRAPH_PASSWORD", value: null }
        { name: "GRAPH_ENDPOINT", value: "bolt+s://db:7687" }
    ]
}

def main [] {
    let tests = {
        "a workload's real environment passes the real checker": {
            let entries = (sound_entries | append [
                { name: "POLAR_SHUTDOWN_BUDGET_SECS", value: "45" }
                { name: "POLAR_HEALTH_TICK_SECS", value: "30" }
                { name: "POLAR_HEALTH_REJUVENATION_SECS", value: "300" }
            ])
            assert equal (environment_problems "w" $entries) []
        }

        "a variable that comes from a secret counts as present": {
            # GRAPH_PASSWORD has no value at render time; it is in sound_entries as a secret.
            let secret = (sound_entries | where name == "GRAPH_PASSWORD" | first)
            assert ($secret.value == null)
            assert equal (environment_problems "w" (sound_entries)) []
        }

        "a capability the workload is given must come with the environment it needs": {
            let entries = [{ name: "POLAR_CAPABILITIES", value: "cassini,graph-write" }]
            let p = (environment_problems "kube-consumer" $entries)
            assert (has $p "BROKER_ADDR")
            assert (has $p "GRAPH_ENDPOINT")
            assert ($p | all {|x| $x | str starts-with "kube-consumer: " })
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
