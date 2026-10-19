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

# Which registry the nodes are told to pull from: the environment first, then the target, then the default.

use std/assert
use ../registry.nu [node_registry, pick_registry, target_push_registry, target_registry]

def main [] {
    let tests = [
        {
            name: "the environment wins over the target, and the target over the default; blanks say nothing"
            body: {
                assert equal (pick_registry "env:5000" "target:5000" "default:5000") "env:5000"
                assert equal (pick_registry "" "target:5000" "default:5000") "target:5000"
                assert equal (pick_registry "" null "default:5000") "default:5000"
                assert equal (pick_registry "  " "" "default:5000") "default:5000"
            }
        }
        {
            name: "a target that keeps its registry in the cluster names it on the nodes' localhost, at its NodePort"
            body: {
                assert equal (target_registry { registry_in_cluster: { node_port: 30500 } }) "localhost:30500"
                assert equal (target_registry { registry: "registry.example.org:5000" }) "registry.example.org:5000"
                assert equal (target_registry {}) null
                let both = (try { target_registry { registry: "x:5000", registry_in_cluster: { node_port: 30500 } }; "accepted" } catch {|e| $e.msg })
                assert ($both | str contains "keep one") $both
            }
        }
        {
            name: "this machine pushes to a registry in the cluster through its forward, and to the default otherwise"
            body: {
                assert equal (target_push_registry { registry_in_cluster: { node_port: 30500 } }) "localhost:5000"
                assert equal (target_push_registry { registry_in_cluster: { node_port: 30500, local_port: 5001 } }) "localhost:5001"
                assert equal (target_push_registry { registry: "registry.example.org:5000" }) null
            }
        }
        {
            name: "a trailing slash is not part of the registry, so a prefix can be built from it"
            body: {
                assert equal (pick_registry "polar-registry:5000/" null "d") "polar-registry:5000"
                assert equal (pick_registry "" null "10.0.2.2:5000//") "10.0.2.2:5000"
            }
        }
        {
            name: "the default is what the repository says when nothing else does, and the environment overrides it"
            body: {
                let base = (with-env { POLAR_NODE_REGISTRY: "" } { node_registry })
                assert equal $base (open ($env.PWD | path join "infra" "config" "cluster.nuon") | get registry)
                assert equal (with-env { POLAR_NODE_REGISTRY: "polar-registry:5000" } { node_registry "local-minimal" }) "polar-registry:5000"
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
