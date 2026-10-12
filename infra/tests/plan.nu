#!/usr/bin/env nu
# infra/tests/up.nu
#
# Tests of what up.nu (infra/plan.nu) decides to run for a target.
#
#   nu infra/tests/up.nu

use std/assert
use ../plan.nu [build_forwards, build_plan, deployed_charts]

def config [charts: list, order: list] {
    { charts: ($charts | each {|c| $"/layers/x/($c)/main.nu" }), apply_order: $order, cert_issuer_ca_configmap: "polar-ca" }
}

def on [plan: list, id: string] {
    $plan | where id == $id | first | get on
}

def main [] {
    let tests = [
        {
            name: "a workload that is in the apply order but not among the charts is deployed"
            body: {
                let c = (config ["cert-issuer" "neo4j"] ["namespaces.yaml" "credentials-agent-resources.yaml" "credentials-agent.yaml" "polar-git-server.yaml"])
                let d = (deployed_charts $c)
                assert ("credentials-agent" in $d)
                assert ("polar-git-server" in $d)
                assert ("cert-issuer" in $d)
            }
        }
        {
            name: "the admin flow runs when the credentials agent is deployed through the catalog"
            body: {
                let plan = (build_plan (config ["neo4j"] ["namespaces.yaml" "credentials-agent.yaml"]) false)
                assert (on $plan "admin")
                assert (on $plan "browser")
            }
        }
        {
            name: "the seed runs only with the credentials agent, the git server and the scheduler's observer all deployed"
            body: {
                let all = ["namespaces.yaml" "credentials-agent.yaml" "polar-git-server.yaml" "polar-scheduler-observer.yaml"]
                assert (on (build_plan (config [] $all) false) "seed")
                for missing in ["credentials-agent.yaml" "polar-git-server.yaml" "polar-scheduler-observer.yaml"] {
                    let order = ($all | where {|f| $f != $missing })
                    assert (not (on (build_plan (config [] $order) false) "seed")) $"seed ran without ($missing)"
                }
            }
        }
        {
            name: "the seed comes after the admin flow, which makes the account that signs it"
            body: {
                let plan = (build_plan (config [] ["credentials-agent.yaml" "polar-git-server.yaml" "polar-scheduler-observer.yaml"]) false)
                let ids = ($plan | get id)
                assert (($ids | enumerate | where item == "seed" | first | get index) > ($ids | enumerate | where item == "admin" | first | get index))
            }
        }
        {
            name: "the git server is port-forwarded when the seed will push to it"
            body: {
                let c = (config [] ["credentials-agent.yaml" "polar-git-server.yaml" "polar-scheduler-observer.yaml"])
                assert ("git" in (build_forwards $c | get name))
                assert ("git" not-in (build_forwards (config [] ["credentials-agent.yaml"]) | get name))
            }
        }
    ]

    let results = ($tests | each {|t|
        let r = (try { do $t.body; null } catch {|e| $e.msg })
        { name: $t.name, error: $r }
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

