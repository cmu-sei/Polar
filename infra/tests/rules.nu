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
        "a workload that asks cert-issuer and is not a bootstrap identity is pending until the floor is enforced, then an error": {
            let r = [{ who: "kube-consumer", issuer: "cert-issuer", bootstrap: false }]
            let before = (issuer_problems $r false)
            assert equal $before.errors []
            assert (has $before.pending "kube-consumer")
            let after = (issuer_problems $r true)
            assert (has $after.errors "kube-consumer")
            assert equal $after.pending []
        }

        "a bootstrap identity may ask cert-issuer, and a workload on the credentials agent is never a problem": {
            let r = [
                { who: "cassini", issuer: "cert-issuer", bootstrap: true }
                { who: "kube-consumer", issuer: "credentials-agent", bootstrap: false }
            ]
            for enforced in [false true] {
                let p = (issuer_problems $r $enforced)
                assert equal $p.errors []
                assert equal $p.pending []
            }
        }

        "a bootstrap identity cannot be on the credentials agent, and an unknown issuer is an error": {
            let p = (issuer_problems [{ who: "cassini", issuer: "credentials-agent", bootstrap: true }] false)
            assert (has $p.errors "cassini")
            let q = (issuer_problems [{ who: "x", issuer: "vault", bootstrap: false }] false)
            assert (has $q.errors "vault")
        }

        "the floor is the bootstrap identities with their type and names, from what they are issued": {
            let r = [
                { who: "cassini", namespace: "polar", service_account: "cassini", issuer: "cert-issuer", bootstrap: true,
                  certs: ["server:/etc/tls/certs:ecdsa-p256:cassini-ip-svc.polar.svc.cluster.local"] }
                { who: "credentials-agent", namespace: "polar", service_account: "credentials-agent", issuer: "cert-issuer", bootstrap: true,
                  certs: ["client:/etc/tls/certs:ecdsa-p256:", "client:/etc/neo4j-client-tls:ecdsa-p256:"] }
            ]
            let f = (floor_entries $r)
            assert equal $f.problems []
            assert equal $f.entries [
                { workload: "system:serviceaccount:polar:cassini", certType: "SERVER", names: ["cassini-ip-svc.polar.svc.cluster.local"], vouchedOnly: false }
                { workload: "system:serviceaccount:polar:credentials-agent", certType: "CLIENT", names: [], vouchedOnly: false }
            ]
        }

        "a workload on the credentials agent gets a vouched-only entry for its server names, and none for client certificates": {
            let r = [
                { who: "polar-git-server", namespace: "polar", service_account: "polar-git-server", issuer: "credentials-agent", bootstrap: false,
                  certs: ["client:/etc/tls/certs:ecdsa-p256:", "server:/etc/tls/git-server:ecdsa-p256:polar-git-server.polar.svc.cluster.local"] }
                { who: "kube-consumer", namespace: "polar", service_account: "kube-consumer", issuer: "credentials-agent", bootstrap: false,
                  certs: ["client:/etc/tls/certs:ecdsa-p256:"] }
            ]
            let f = (floor_entries $r)
            assert equal $f.problems []
            assert equal $f.entries [
                { workload: "system:serviceaccount:polar:polar-git-server", certType: "SERVER", names: ["polar-git-server.polar.svc.cluster.local"], vouchedOnly: true }
            ]
        }

        "a bootstrap identity that is issued two types cannot be written as one floor entry": {
            let r = [{ who: "x", namespace: "polar", service_account: "x", issuer: "cert-issuer", bootstrap: true,
                       certs: ["client:/a:ecdsa-p256:", "server:/b:ecdsa-p256:x.polar"] }]
            let f = (floor_entries $r)
            assert (has $f.problems "x")
        }

        "a workload still on cert-issuer that is not a bootstrap identity has no floor entry yet": {
            # Its names stay unchecked until it moves; the issuer rule is what reports it.
            let r = [{ who: "k", namespace: "polar", service_account: "k", issuer: "cert-issuer", bootstrap: false,
                       certs: ["server:/b:ecdsa-p256:k.polar"] }]
            assert equal (floor_entries $r).entries []
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
