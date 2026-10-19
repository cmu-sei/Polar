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

# The inputs that belong to one cluster: how they are added, when they are discarded, and how they meet the target's own env.

use std/assert
use ../cluster-inputs.nu [admins_of, empty, env_for, env_merged, path_of, reconcile, with_admin]

def main [] {
    let tests = [
        {
            name: "the file sits beside the target's config"
            body: { assert equal (path_of "/r/infra" "sandbox-local") "/r/infra/targets/sandbox-local/cluster-inputs.nuon" }
        }
        {
            name: "an administrator is added to a cluster's inputs once, and a second one beside it"
            body: {
                let a = (with_admin (empty) "id-1" "ca-A")
                assert $a.changed
                assert equal (admins_of $a.inputs) ["id-1"]
                assert equal $a.inputs.cluster "ca-A"
                let again = (with_admin $a.inputs "id-1" "ca-A")
                assert (not $again.changed)
                assert equal $again.inputs $a.inputs
                let b = (with_admin $a.inputs "id-2" "ca-A")
                assert equal (admins_of $b.inputs) ["id-1" "id-2"]
            }
        }
        {
            name: "inputs made for another cluster are discarded, not extended"
            body: {
                let old = (with_admin (empty) "old-id" "ca-A").inputs
                let fresh = (with_admin $old "new-id" "ca-B")
                assert $fresh.changed
                assert equal (admins_of $fresh.inputs) ["new-id"]
                assert equal $fresh.inputs.cluster "ca-B"
                assert equal (reconcile $old "ca-B") (empty)
                assert equal (reconcile $old "ca-A") $old
                # no cluster yet: whatever was left over belongs to none
                assert equal (reconcile $old null) (empty)
            }
        }
        {
            name: "the env a workload is given comes from the inputs, and nothing is given to another workload"
            body: {
                let i = (with_admin (empty) "id-1" "ca-A").inputs
                assert equal (env_for $i "polar-git-server" | first | get value) "id-1"
                assert equal (env_for $i "cassini") []
                assert equal (env_for (empty) "polar-git-server") []
            }
        }
        {
            name: "a value the cluster supplies replaces the target's own of that name, and the rest stay"
            body: {
                let config = [{ name: "POLAR_GIT_SERVER_ADMINS", value: "stale-id" } { name: "OTHER", value: "x" }]
                let merged = (env_merged $config [{ name: "POLAR_GIT_SERVER_ADMINS", value: "id-1" }])
                assert equal ($merged | where name == "POLAR_GIT_SERVER_ADMINS" | length) 1
                assert equal ($merged | where name == "POLAR_GIT_SERVER_ADMINS" | first | get value) "id-1"
                assert equal ($merged | where name == "OTHER" | first | get value) "x"
                assert equal (env_merged $config []) $config
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
