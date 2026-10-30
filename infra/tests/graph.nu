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

# The catalog's graph schema (infra/graph.nu): who reads, the state agent's floor, and the reads of a label nobody owns.
#
#   nu infra/tests/graph.nu

use std/assert
use ../graph.nu [readers, unowned_reads, state_floor, over_readers]

def g [label: string, access: string] { { label: $label, access: $access, purpose: "for a test" } }

def set [workload: string, graph: list] { { workload: $workload, graph: $graph } }

def record_of [who: string, graph: list, service_account?: string] {
    { who: $who, namespace: "polar", service_account: ($service_account | default $who), graph: $graph }
}

def main [] {
    let tests = [
        {
            name: "the readers are the workloads that read a label, by name, each once"
            body: {
                let sets = [
                    (set "jira-processor" [(g "JiraIssue" "owns") (g "JiraUser" "owns")])
                    (set "jira-observer" [(g "JiraIssue" "reads") (g "JiraUser" "reads")])
                    (set "git-repo-observer" [(g "GitRepository" "reads")])
                    (set "kube-observer" [])
                    { workload: "cassini" }
                ]
                assert equal (readers $sets) "git-repo-observer,jira-observer"
                assert equal (readers []) ""
            }
        }
        {
            name: "a read of a label no workload owns is named, with its reader, and an owned one is not"
            body: {
                let sets = [
                    (set "jira-processor" [(g "JiraIssue" "owns")])
                    (set "jira-observer" [(g "JiraIssue" "reads") (g "JiraUser" "reads")])
                    (set "git-repo-observer" [(g "GitRepository" "reads") (g "GitRepository" "reads")])
                ]
                assert equal (unowned_reads $sets) [
                    { workload: "jira-observer", label: "JiraUser" }
                    { workload: "git-repo-observer", label: "GitRepository" }
                ]
                # A reader that owns the label itself has another side.
                assert equal (unowned_reads [(set "a" [(g "X" "owns") (g "X" "reads")])]) []
                assert equal (unowned_reads []) []
            }
        }
        {
            name: "the floor lists every workload that declares a label, by its identity, owns and reads apart"
            body: {
                let floor = (state_floor [
                    (record_of "jira-observer" [(g "JiraIssue" "reads")])
                    (record_of "kube-observer" [])
                    (record_of "jira-processor" [(g "JiraUser" "owns") (g "JiraIssue" "owns") (g "JiraIssue" "owns")])
                ] "cluster.local")
                assert equal $floor {
                    format: 1
                    workloads: [
                        { workload: "jira-observer", identity: "jira-observer.polar.serviceaccount.cluster.local", owns: [], reads: ["JiraIssue"] }
                        { workload: "jira-processor", identity: "jira-processor.polar.serviceaccount.cluster.local", owns: ["JiraIssue" "JiraUser"], reads: [] }
                    ]
                }
                assert equal (state_floor [] "cluster.local") { format: 1, workloads: [] }
            }
        }
        {
            name: "a workload whose service account is not its name has no place in the floor"
            body: {
                let r = (try { state_floor [(record_of "jira-observer" [(g "JiraIssue" "reads")] "other")] "cluster.local"; "made" } catch {|e| $e.msg })
                assert ($r | str contains "jira-observer (service account other)") $r
            }
        }
        {
            name: "a scaffold with a family over the readers is told apart from one over the served workloads"
            body: {
                assert (over_readers { topic_families: [{ pattern: "soft-state-replies.{workload}", over: "POLAR_STATE_READERS" }] })
                assert not (over_readers { topic_families: [{ pattern: "agent.{workload}.configuration", over: "POLAR_SERVED_WORKLOADS" }] })
                assert not (over_readers {})
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
