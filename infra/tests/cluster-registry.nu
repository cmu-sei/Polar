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

# The registry in the target's cluster (infra/cluster-registry.nu): its settings, its manifests, and where the storage chart's images
# come from. The pure parts only; `up.nu` applies it.

use std/assert
use ../cluster-registry.nu [manifests, settings_of, storage_images, NAMESPACE, REGISTRY_IMAGE, UPSTREAM_STORAGE_IMAGES]

const INFRA = (path self | path dirname | path dirname)

def cluster [rc: any] { { storage: { class: "managed-csi", size: "1Gi" } } | merge (if $rc == null { {} } else { { registry_in_cluster: $rc } }) }

def main [] {
    let tests = [
        {
            name: "a target keeps its registry in the cluster only when it says so, and its settings default the rest"
            body: {
                assert equal (settings_of (cluster null)) null
                assert equal (settings_of (cluster { node_port: 30500 })) { node_port: 30500, local_port: 5000, size: "20Gi", storage_class: "managed-csi" }
                assert equal (settings_of (cluster { node_port: 30500, local_port: 5001, size: "50Gi" })).local_port 5001
                for bad in [80 29999 32768 "30500"] {
                    let msg = (try { settings_of (cluster { node_port: $bad }); "accepted" } catch {|e| $e.msg })
                    assert ($msg | str contains "between 30000 and 32767") $"($bad): ($msg)"
                }
            }
        }
        {
            name: "the registry is a NodePort at the target's port, its images on a claim of the target's storage class"
            body: {
                let m = (manifests (settings_of (cluster { node_port: 30500, size: "30Gi" })))
                let kind = {|k| $m | where kind == $k | first }
                assert equal (do $kind "Namespace").metadata.name $NAMESPACE
                let svc = (do $kind "Service")
                assert equal $svc.spec.type "NodePort"
                assert equal ($svc.spec.ports | first) { port: 5000, targetPort: 5000, nodePort: 30500 }
                let pvc = (do $kind "PersistentVolumeClaim")
                assert equal $pvc.spec.storageClassName "managed-csi"
                assert equal $pvc.spec.resources.requests.storage "30Gi"
                let d = (do $kind "Deployment")
                assert equal $d.spec.strategy.type "Recreate" "a ReadWriteOnce volume is held by one pod at a time"
                let c = ($d.spec.template.spec.containers | first)
                assert equal ($c.volumeMounts | first).mountPath "/var/lib/registry"
                assert equal ($d.spec.template.spec.volumes | first).persistentVolumeClaim.claimName $pvc.metadata.name
                assert equal $svc.spec.selector $d.spec.selector.matchLabels
                assert equal ($m | each {|o| $o.metadata.namespace? | default $NAMESPACE } | uniq) [$NAMESPACE]
            }
        }
        {
            name: "what runs before the registry exists comes from upstream, pinned by digest"
            body: {
                assert ($REGISTRY_IMAGE | str starts-with "docker.io/library/registry:")
                assert ($REGISTRY_IMAGE =~ '@sha256:[0-9a-f]{64}$')
                for to in ($UPSTREAM_STORAGE_IMAGES | values) { assert ($to =~ '^docker\.io/.+@sha256:[0-9a-f]{64}$') $to }
            }
        }
        {
            name: "the storage chart's images come from the node registry, or from upstream when the registry is in the cluster"
            body: {
                let text = "image: node-registry.invalid/local-path-provisioner:v0.0.35\nhelper: node-registry.invalid/busybox:latest\n"
                assert equal (storage_images $text "10.0.2.2:5000" false) "image: 10.0.2.2:5000/local-path-provisioner:v0.0.35\nhelper: 10.0.2.2:5000/busybox:latest\n"
                let up = (storage_images $text "localhost:30500" true)
                assert (not ($up | str contains "node-registry.invalid")) $up
                assert (not ($up | str contains "localhost:30500")) "nothing comes from the registry the provisioner's volume holds"
                assert ($up | str contains "docker.io/rancher/local-path-provisioner:v0.0.35@sha256:")
                assert ($up | str contains "docker.io/library/busybox:1.37.0@sha256:")
            }
        }
        {
            name: "every image placeholder in the storage chart's manifest has an upstream, so none is left behind"
            body: {
                let text = (open --raw ($INFRA | path join "layers/1-platform/infra/storage/local-path-provisioner.yaml"))
                let found = ($text | parse --regex '(?<ref>node-registry\.invalid/[^\s"]+)' | get ref | uniq | sort)
                assert equal $found ($UPSTREAM_STORAGE_IMAGES | columns | sort)
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
