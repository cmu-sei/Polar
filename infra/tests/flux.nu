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

# What `flux.nu install` and `wait` decide: where the nodes reach the deployment git server, and when every layer
# counts as done.

use std/assert
use ../flux.nu [commit_of_revision, due_stale, revision_waited_on, stale_waits, track_stale, describe_state, layer_states, settled]

def kustomization [name: string, ready: string, revision: string, reason: string = "ReconciliationSucceeded", message: string = ""] {
    {
        metadata: { name: $name }
        status: {
            lastAppliedRevision: $revision
            conditions: [{ type: "Ready", status: $ready, reason: $reason, message: $message }]
        }
    }
}


# A Kustomization in the middle of a reconcile: generation of the spec, and the Reconciling condition it began under.
def reconciling [name: string, generation: int, began_under: int, message: string] {
    {
        metadata: { name: $name, generation: $generation }
        status: { conditions: [{ type: "Reconciling", status: "True", reason: "Progressing", message: $message, observedGeneration: $began_under }] }
    }
}

const WAITING = "Running health checks for revision main@sha1:abc with a timeout of 10m0s"

def main [] {
    let tests = [
        {
            name: "a revision is reduced to its commit"
            body: { assert equal (commit_of_revision "main@sha1:abc123") "abc123" }
        }
        {
            name: "a layer is done when it is Ready and was applied from the published commit, not an earlier one"
            body: {
                let states = (layer_states [
                    (kustomization "polar-platform" "True" "main@sha1:abc")
                    (kustomization "polar-data" "True" "main@sha1:old")
                    (kustomization "polar-cassini" "False" "" "DependencyNotReady" "dependency polar-data is not ready\nmore")
                ] "abc")
                assert equal ($states | get current) [true false false]
                assert equal ($states | get ready) [true true false]
                assert (not (settled $states))
                assert (settled (layer_states [(kustomization "a" "True" "main@sha1:abc") (kustomization "b" "True" "main@sha1:abc")] "abc"))
            }
        }
        {
            name: "nothing listed is not done, and a Kustomization with no status yet is not ready"
            body: {
                assert (not (settled []))
                let s = (layer_states [{ metadata: { name: "polar-x" } }] "abc")
                assert (not ($s | first | get ready))
                assert (not (settled $s))
            }
        }
        {
            name: "the line for a layer says what it waits for, first line only"
            body: {
                let states = (layer_states [(kustomization "polar-cassini" "False" "" "DependencyNotReady" "dependency polar-data is not ready\nsecond line")] "abc")
                let line = (describe_state ($states | first))
                assert ($line | str contains "polar-cassini: DependencyNotReady")
                assert ($line | str contains "dependency polar-data is not ready")
                assert (not ($line | str contains "second line"))
                let old = (layer_states [(kustomization "polar-data" "True" "main@sha1:old")] "abc")
                assert ((describe_state ($old | first)) | str contains "ready, but at main@sha1:old")
                let done = (layer_states [(kustomization "polar-data" "True" "main@sha1:abc")] "abc")
                assert equal (describe_state ($done | first)) "polar-data: ready"
            }
        }
        {
            name: "the revision a health-check wait was begun for is read from its message"
            body: {
                assert equal (revision_waited_on $WAITING) "main@sha1:abc"
                assert equal (revision_waited_on "Reconciliation in progress") null
            }
        }
        {
            name: "a wait begun under an older spec or revision is stale; one begun under the current ones is not"
            body: {
                # spec changed since the wait began (generation 2, began under 1)
                assert equal (stale_waits [(reconciling "polar-identity" 2 1 $WAITING)] "abc") ["polar-identity"]
                # the published commit moved on
                assert equal (stale_waits [(reconciling "polar-identity" 1 1 $WAITING)] "def") ["polar-identity"]
                # current spec, current revision: an ordinary wait
                assert equal (stale_waits [(reconciling "polar-identity" 1 1 $WAITING)] "abc") []
            }
        }
        {
            name: "only a health-check wait can be stale; waiting on a dependency, or being Ready, cannot"
            body: {
                let dependency = (reconciling "polar-agents" 3 1 "dependency 'flux-system/polar-identity' is not ready")
                let ready = { metadata: { name: "polar-platform", generation: 2 }, status: { conditions: [{ type: "Ready", status: "True", message: "Applied revision: main@sha1:abc", observedGeneration: 1 }] } }
                let none = { metadata: { name: "polar-new", generation: 1 } }
                assert equal (stale_waits [$dependency $ready $none] "abc") []
                # several at once: only the stale one
                let mixed = (stale_waits [(reconciling "a" 2 1 $WAITING) (reconciling "b" 1 1 $WAITING) $ready] "abc")
                assert equal $mixed ["a"]
            }
        }
        {
            name: "a Kustomization is acted on only after it has been stale for the grace period, and forgotten once it is not"
            body: {
                let t0 = ("2026-10-19T12:00:00Z" | into datetime)
                let seen = (track_stale {} ["a"] $t0)
                assert equal ($seen | columns) ["a"]
                # still stale later: the first sighting is kept
                let later = ($t0 + 10sec)
                let again = (track_stale $seen ["a" "b"] $later)
                assert equal ($again | get a) $t0
                assert equal ($again | get b) $later
                assert equal (due_stale $again $later 15sec) []
                assert equal (due_stale $again ($t0 + 16sec) 15sec) ["a"]
                # no longer stale: forgotten, so a later stale spell starts its own clock
                assert equal (track_stale $again [] ($t0 + 20sec)) {}
            }
        }
    ]

    let results = ($tests | each {|t|
        let r = (try { do $t.body; null } catch {|e| $e.msg })
        { name: $t.name, error: $r }
    })
    for r in $results {
        if $r.error == null { print $"  ok    ($r.name)" } else { print -e $"  FAIL  ($r.name): ($r.error)" }
    }
    let failed = ($results | where error != null | length)
    if $failed > 0 {
        print -e $"($failed) of ($results | length) failed"
        exit 1
    }
    print $"all ($results | length) passed"
}
