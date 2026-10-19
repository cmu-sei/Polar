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
use ../flux.nu [commit_of_revision, describe_state, host_documents, is_ipv4, layer_states, service_name, settled]

def kustomization [name: string, ready: string, revision: string, reason: string = "ReconciliationSucceeded", message: string = ""] {
    {
        metadata: { name: $name }
        status: {
            lastAppliedRevision: $revision
            conditions: [{ type: "Ready", status: $ready, reason: $reason, message: $message }]
        }
    }
}

def main [] {
    let tests = [
        {
            name: "only an IPv4 address is taken as the host, since an EndpointSlice cannot hold a name"
            body: {
                assert (is_ipv4 "10.0.2.2")
                assert (not (is_ipv4 "10.0.2"))
                assert (not (is_ipv4 "10.0.2.256"))
                assert (not (is_ipv4 "host.example"))
                assert (not (is_ipv4 "10.0.2.2.5"))
            }
        }
        {
            name: "the Service has the first label of the certificate's name, and the EndpointSlice leads it to the host"
            body: {
                assert equal (service_name) "polar-deploy-git"
                let docs = (host_documents "10.0.2.2" 9443)
                let svc = ($docs | where kind == "Service" | first)
                let slice = ($docs | where kind == "EndpointSlice" | first)
                assert equal $svc.metadata.name "polar-deploy-git"
                assert equal $svc.metadata.namespace "flux-system"
                assert (($svc.spec | get -o selector) == null)
                assert equal $svc.spec.ports.0.port 9443
                assert equal ($slice.metadata.labels | get "kubernetes.io/service-name") "polar-deploy-git"
                assert equal $slice.endpoints.0.addresses ["10.0.2.2"]
                assert equal $slice.ports.0.port 9443
            }
        }
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
