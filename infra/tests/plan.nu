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
            name: "the seed comes after the admin flow, which makes the account that signs it, and before the browser, which waits for a person"
            body: {
                let plan = (build_plan (config [] ["credentials-agent.yaml" "polar-git-server.yaml" "polar-scheduler-observer.yaml"]) false)
                let ids = ($plan | get id)
                let at = {|id| $ids | enumerate | where item == $id | first | get index }
                assert ((do $at "seed") > (do $at "admin"))
                let full = (build_plan (config ["neo4j"] ["credentials-agent.yaml" "polar-git-server.yaml" "polar-scheduler-observer.yaml"]) false | get id)
                assert (($full | enumerate | where item == "seed" | first | get index) < ($full | enumerate | where item == "browser" | first | get index))
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
        {
            name: "a target that is not gitops applies its layers itself, and has no deployment host, publish or Flux steps"
            body: {
                let plan = (build_plan (config ["neo4j"] ["namespaces.yaml" "credentials-agent.yaml"]) false)
                assert (on $plan "infra")
                for id in ["secrets" "deploy-host" "publish" "flux" "flux-wait"] { assert (not (on $plan $id)) }
            }
        }
        {
            name: "a gitops target creates its secrets, publishes and waits for Flux in place of applying, and still runs the steps that need workloads"
            body: {
                let c = ((config ["neo4j"] ["namespaces.yaml" "credentials-agent.yaml" "polar-git-server.yaml" "polar-scheduler-observer.yaml"]) | insert gitops true)
                let plan = (build_plan $c false)
                assert (not (on $plan "infra"))
                for id in ["secrets" "deploy-host" "publish" "flux" "flux-wait" "admin" "seed" "browser"] { assert (on $plan $id) }
                # Flux comes after the CA and the secrets it does not own, and the workload steps after Flux
                let order = ($plan | get id)
                assert (($order | enumerate | where item == "ca" | first | get index) < ($order | enumerate | where item == "secrets" | first | get index))
                assert (($order | enumerate | where item == "secrets" | first | get index) < ($order | enumerate | where item == "publish" | first | get index))
                assert (($order | enumerate | where item == "flux-wait" | first | get index) < ($order | enumerate | where item == "admin" | first | get index))
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

