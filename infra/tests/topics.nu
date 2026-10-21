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

# The catalog's topics (infra/topics.nu): the workloads a family serves, and the declared topics with no other side.

use std/assert
use ../topics.nu [served, one_sided, judge]

def t [name: string, access: string, from: string, waits?: string] {
    let base = { name: $name, access: $access, from: $from }
    if $waits == null { $base } else { $base | insert waits $waits }
}

def set [workload: string, topics: list] { { workload: $workload, topics: $topics } }

def main [] {
    let tests = [
        {
            name: "a family serves the catalog's agents that read a document, by name, and not a service"
            body: {
                let workloads = [
                    { name: "kube-observer", kind: "agent", config: "document" }
                    { name: "cassini", kind: "service", config: "document" }
                    { name: "a-legacy", kind: "agent" }
                    { name: "credentials-agent", kind: "agent", config: "document" }
                ]
                assert equal (served $workloads) "credentials-agent,kube-observer"
            }
        }
        {
            name: "a declared topic is one-sided when nothing publishes what it subscribes, or reads what it publishes"
            body: {
                let sets = [
                    (set "observer" [(t "events" "publish" "declared") (t "unread" "publish" "declared")])
                    (set "consumer" [(t "events" "subscribe" "declared") (t "never-sent" "subscribe" "declared")])
                ]
                let found = (one_sided $sets | select workload access topic)
                assert equal $found [
                    { workload: "observer", access: "publish", topic: "unread" }
                    { workload: "consumer", access: "subscribe", topic: "never-sent" }
                ]
            }
        }
        {
            name: "an implied topic is another side, and is never itself one-sided"
            body: {
                let sets = [
                    (set "credentials-agent" [(t "polar-authority-updates" "publish" "declared")])
                    (set "kube-observer" [(t "polar-authority-updates" "subscribe" "implied") (t "polar-configuration-replay" "publish" "implied")])
                ]
                assert equal (one_sided $sets) []
            }
        }
        {
            name: "a family topic needs its other side as a declared one does"
            body: {
                let sets = [
                    (set "polar-scheduler" [(t "agent.kube-observer.configuration" "publish" "family") (t "agent.gone.configuration" "publish" "family")])
                    (set "kube-observer" [(t "agent.kube-observer.configuration" "subscribe" "implied")])
                ]
                assert equal (one_sided $sets | get topic) ["agent.gone.configuration"]
            }
        }
        {
            name: "an agent on both sides of its own topic is not one-sided"
            body: {
                assert equal (one_sided [(set "a" [(t "x" "publish" "declared") (t "x" "subscribe" "declared")])]) []
            }
        }
        {
            name: "the judgement fails a one-sided topic with no reason, and lists one that says why it waits"
            body: {
                let sets = [
                    (set "resolver" [(t "polar.provenance.events" "publish" "declared" "read by build-processor, a chart")])
                    (set "kube-consumer" [(t "polar.provenance.discovery" "publish" "declared")])
                ]
                let j = (judge $sets)
                assert equal ($j.failures | get topic) ["polar.provenance.discovery"]
                assert equal ($j.waiting | get topic) ["polar.provenance.events"]
                assert equal ($j.waiting | first | get waits) "read by build-processor, a chart"
                assert equal (judge [(set "a" [(t "x" "publish" "declared")]) (set "b" [(t "x" "subscribe" "declared")])]).failures []
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
