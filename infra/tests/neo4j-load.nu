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

# The loader pod and what it runs, and how the Flux Kustomization that applies Neo4j is found. The load itself needs a cluster:
# infra/neo4j-load.nu's header says what it does.

use std/assert
use ../neo4j-load.nu [load_command, loader_pod, owning_kustomization]

def main [] {
    let tests = [
        {
            name: "the Kustomization that applies the StatefulSet is read from Flux's labels, and there is none without them"
            body: {
                assert equal (owning_kustomization { "kustomize.toolkit.fluxcd.io/name": "polar-data", "kustomize.toolkit.fluxcd.io/namespace": "flux-system" }) { name: "polar-data", namespace: "flux-system" }
                assert equal (owning_kustomization { "kustomize.toolkit.fluxcd.io/name": "polar-data" }) { name: "polar-data", namespace: "flux-system" }
                assert equal (owning_kustomization { app: "neo4j" }) null
            }
        }
        {
            name: "the loader runs Neo4j's image as Neo4j's user, with the data volume where Neo4j has it and a /tmp of its own"
            body: {
                let p = (loader_pod "localhost:30500/neo4j:5.26.2@sha256:abc")
                let c = ($p.spec.containers | first)
                assert equal $c.image "localhost:30500/neo4j:5.26.2@sha256:abc"
                assert equal $p.spec.securityContext.runAsUser 7474
                assert equal $p.spec.securityContext.fsGroup 7474
                assert ($c.volumeMounts | any {|m| $m.mountPath == "/var/lib/neo4j/data" and $m.name == "data" })
                assert ($c.volumeMounts | any {|m| $m.mountPath == "/tmp" })
                assert equal ($p.spec.volumes | where name == "data" | first | get persistentVolumeClaim.claimName) "polar-db-data"
            }
        }
        {
            name: "the load replaces the neo4j database only, from /dump"
            body: {
                let cmd = (load_command)
                assert ($cmd | str contains "neo4j-admin database load neo4j --from-path=/dump --overwrite-destination=true")
                assert ($cmd | str contains "server.directories.data=/var/lib/neo4j/data")
                assert not ($cmd | str contains "system")
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
