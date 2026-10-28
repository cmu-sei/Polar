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

# infra/tests/apply.nu
#
# Tests of the decisions in infra/apply.nu that `up.nu` makes while applying manifests: which
# Jobs to leave running on a re-run, and which manifests to wait on before applying the next.
#
#   nu infra/tests/apply.nu

use std/assert
use ../apply.nu *

def main [] {
    let tests = {
        "what kubectl apply left unchanged is not shown; what it created or configured is": {
            assert equal (changed_lines "configmap/a unchanged\ndeployment.apps/b configured\n\nservice/c created\nsecret/d unchanged") ["deployment.apps/b configured" "service/c created"]
            assert equal (changed_lines "configmap/a unchanged\n") []
        }
        "a job whose manifest is unchanged and whose pods are ready is left alone": {
            let v = (job_verdict 0 [{ name: "cassini", ready: true, done: false }])
            assert (not $v.restart)
            assert ($v.reason | str contains "unchanged")
        }
        "a job whose manifest differs is restarted": {
            let v = (job_verdict 1 [{ name: "cassini", ready: true, done: false }])
            assert $v.restart
            assert ($v.reason | str contains "differs")
        }
        "a manifest that could not be compared is restarted, and says so": {
            let v = (job_verdict 2 [])
            assert $v.restart
            assert ($v.reason | str contains "compare")
        }
        "an unchanged job with a pod that is not ready is restarted, naming it": {
            let v = (job_verdict 0 [
                { name: "kube-observer", ready: true, done: false }
                { name: "kube-consumer", ready: false, done: false }
            ])
            assert $v.restart
            assert ($v.reason | str contains "kube-consumer")
            assert (not ($v.reason | str contains "kube-observer"))
        }
        "an unchanged job that has finished is left alone": {
            let v = (job_verdict 0 [{ name: "once", ready: false, done: true }])
            assert (not $v.restart)
        }
        "every job in a file must be fine for the file to be left alone": {
            let v = (job_verdict 0 [
                { name: "a", ready: true, done: false }
                { name: "b", ready: false, done: false }
            ])
            assert $v.restart
        }
        "a job whose image was rebuilt is restarted even when its manifest is unchanged and it is ready": {
            let v = (job_verdict 0 [{ name: "a", ready: true, done: false }] ["x:latest runs sha256:aaa, the registry has sha256:bbb"])
            assert $v.restart
            assert ($v.reason | str contains "image")
            assert ($v.reason | str contains "sha256:bbb")
        }
        "a job whose image was not rebuilt is left alone, however many images were built elsewhere": {
            # --dev builds everything; only a Job whose own image changed has anything to restart for.
            let v = (job_verdict 0 [{ name: "a", ready: true, done: false }] [])
            assert (not $v.restart)
        }
        "a job whose ConfigMap changed is restarted even when its manifest is unchanged and it is ready": {
            let v = (job_verdict 0 [{ name: "a", ready: true, done: false }] [] ["polar-git-server-config"])
            assert $v.restart
            assert ($v.reason | str contains "polar-git-server-config")
        }
        "a ConfigMap is changed when kubectl says it was configured, and not when it was created or is unchanged": {
            let out = "configmap/a-config configured\npersistentvolumeclaim/a-data unchanged\nconfigmap/b-config unchanged\nconfigmap/c-config created\nservice/a configured\n"
            assert equal (changed_configmaps $out) ["a-config"]
            assert equal (changed_configmaps "") []
        }
        "the ConfigMaps a workload mounts or reads are found in its manifest, and only for workloads": {
            let manifest = "apiVersion: batch/v1\nkind: Job\nmetadata:\n  name: a\n  namespace: polar\nspec:\n  template:\n    spec:\n      volumes:\n      - name: cfg\n        configMap:\n          name: a-config\n      containers:\n      - name: a\n        envFrom:\n        - configMapRef:\n            name: a-env\n        env:\n        - name: X\n          valueFrom:\n            configMapKeyRef:\n              name: a-key\n              key: k\n---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: a-config\n"
            let users = (config_users $manifest)
            assert equal ($users | length) 1
            assert equal ($users | first | get kind) "Job"
            assert equal ($users | first | get name) "a"
            assert equal ($users | first | get configmaps | sort) ["a-config" "a-env" "a-key"]
        }
        "a Deployment's and a StatefulSet's ConfigMaps are found too, since a changed ConfigMap does not restart them either": {
            let manifest = "kind: Deployment\nmetadata:\n  name: d\nspec:\n  template:\n    spec:\n      volumes:\n      - name: v\n        configMap:\n          name: d-config\n---\nkind: StatefulSet\nmetadata:\n  name: s\nspec:\n  template:\n    spec:\n      volumes:\n      - name: v\n        configMap:\n          name: s-config\n"
            let users = (config_users $manifest)
            assert equal ($users | get kind | sort) ["Deployment" "StatefulSet"]
        }
        "a Deployment or StatefulSet is rolled when a ConfigMap it uses changed and its own spec did not": {
            # generation only goes up when the spec changes; kubectl prints "configured" for more than that.
            assert (workload_needs_restart 3 3 ["a-config"])
        }
        "a workload whose own spec changed is already rolling, so a changed ConfigMap does not roll it twice": {
            assert (not (workload_needs_restart 3 4 ["a-config"]))
        }
        "a workload that did not exist before is new, not restarted": {
            assert (not (workload_needs_restart null 1 ["a-config"]))
            # nor one that is not there at all, before or after: there is nothing to restart
            assert (not (workload_needs_restart null null ["a-config"]))
        }
        "a workload whose ConfigMaps did not change is not rolled, whatever kubectl printed": {
            assert (not (workload_needs_restart 3 3 []))
        }
        # docs/architecture/deployments.md, step 4: the agents are Deployments, and images keep their tags, so an
        # unchanged Deployment whose pod runs a rebuilt image's old copy is rolled, as a Job in that state was restarted.
        "a Deployment whose image was rebuilt under its tag is rolled, unless its own spec changed": {
            assert (workload_needs_restart 3 3 [] ["kube-consumer: 10.0.2.2:5000/kube-consumer:latest"])
            assert (not (workload_needs_restart 3 4 [] ["kube-consumer: 10.0.2.2:5000/kube-consumer:latest"]))
            assert (not (workload_needs_restart null 1 [] ["kube-consumer: 10.0.2.2:5000/kube-consumer:latest"]))
        }
        "an image is stale when what a pod runs is not what the registry serves, and never when the registry's answer is unknown": {
            let running = [
                { image: "10.0.2.2:5000/a:latest", digest: "sha256:old" }
                { image: "10.0.2.2:5000/b:latest", digest: "sha256:same" }
                { image: "10.0.2.2:5000/c:latest", digest: "sha256:whatever" }
            ]
            let served = [
                { image: "10.0.2.2:5000/a:latest", digest: "sha256:new" }
                { image: "10.0.2.2:5000/b:latest", digest: "sha256:same" }
            ]
            let stale = (stale_images $running $served)
            assert equal ($stale | length) 1
            assert ($stale | first | str contains "10.0.2.2:5000/a:latest")
            assert ($stale | first | str contains "sha256:old")
            assert ($stale | first | str contains "sha256:new")
        }
        "a running container on an image the registry has since replaced is found, named, with both digests; the rest are not": {
            let pod = {|name, phase, statuses, deleting|
                { metadata: ({ name: $name, namespace: "polar" } | merge (if $deleting { { deletionTimestamp: "2026-10-19T00:00:00Z" } } else { {} })), status: { phase: $phase, containerStatuses: $statuses } }
            }
            let old = { name: "cert-issuer", image: "10.0.2.2:5000/cert-issuer:latest", imageID: "10.0.2.2:5000/cert-issuer@sha256:old" }
            let same = { name: "cassini", image: "10.0.2.2:5000/cassini:latest", imageID: "10.0.2.2:5000/cassini@sha256:same" }
            let pinned = { name: "x", image: "10.0.2.2:5000/x:latest@sha256:abc", imageID: "10.0.2.2:5000/x@sha256:abc" }
            let nostatus = { name: "y", image: "10.0.2.2:5000/y:latest", imageID: "" }
            let unknown = { name: "z", image: "10.0.2.2:5000/z:latest", imageID: "10.0.2.2:5000/z@sha256:q" }
            let served = [
                { image: "10.0.2.2:5000/cert-issuer:latest", digest: "sha256:new" }
                { image: "10.0.2.2:5000/cassini:latest", digest: "sha256:same" }
                { image: "10.0.2.2:5000/x:latest@sha256:abc", digest: "sha256:other" }
                { image: "10.0.2.2:5000/y:latest", digest: "sha256:new" }
            ]
            let pods = [
                (do $pod "cert-issuer-1" "Running" [$old $same] false)
                (do $pod "cert-issuer-gone" "Running" [$old] true)
                (do $pod "pending" "Pending" [$old] false)
                (do $pod "other" "Running" [$pinned $nostatus $unknown] false)
            ]
            let stale = (stale_pod_images $pods $served)
            assert equal ($stale | length) 1
            let one = ($stale | first)
            assert equal $one.pod "cert-issuer-1"
            assert equal $one.container "cert-issuer"
            assert equal $one.running "sha256:old"
            assert equal $one.registry "sha256:new"
            assert equal $one.ns "polar"
        }
        "the report names each container with both digests and the image, and says what to do; nothing found is nothing said": {
            assert equal (stale_pod_report []) []
            let lines = (stale_pod_report [{ ns: "polar", pod: "cert-issuer-1", container: "cert-issuer", image: "10.0.2.2:5000/cert-issuer:latest", running: "sha256:0123456789abcdef", registry: "sha256:fedcba9876543210" }])
            assert ($lines.0 | str contains "1 running container")
            assert ($lines.1 | str contains "polar/cert-issuer-1")
            assert ($lines.1 | str contains "0123456789a")
            assert ($lines.1 | str contains "fedcba98765")
            assert ($lines.1 | str contains "10.0.2.2:5000/cert-issuer:latest")
            assert (($lines | last) | str contains "minikube image rm")
        }
        "the jobs in a list of objects are found, and only the jobs": {
            let list = { kind: "List", items: [
                { kind: "Job", metadata: { name: "a", namespace: "polar" }, status: { succeeded: 1 } }
                { kind: "ConfigMap", metadata: { name: "c", namespace: "polar" } }
                { kind: "Job", metadata: { name: "b", namespace: "polar" }, status: {} }
            ] }
            let jobs = (jobs_in $list)
            assert equal ($jobs | get name) ["a" "b"]
            assert equal ($jobs | get done) [true false]
        }
        "a single object that is a job is found as one": {
            let jobs = (jobs_in { kind: "Job", metadata: { name: "solo", namespace: "polar" } })
            assert equal ($jobs | get name) ["solo"]
        }
        "a single object that is not a job has no jobs": {
            assert equal (jobs_in { kind: "Deployment", metadata: { name: "d", namespace: "polar" } }) []
        }
        "a pod that is ready but being deleted does not count as ready": {
            let old = { metadata: { deletionTimestamp: "2026-10-08T00:00:00Z" }, status: { phase: "Running", containerStatuses: [{ ready: true }] } }
            assert (not (any_pod_ready [$old]))
        }
        "a ready pod beside a deleted one counts": {
            let old = { metadata: { deletionTimestamp: "2026-10-08T00:00:00Z" }, status: { phase: "Running", containerStatuses: [{ ready: true }] } }
            let new = { metadata: {}, status: { phase: "Running", containerStatuses: [{ ready: true }] } }
            assert (any_pod_ready [$old $new])
        }
        "a running pod with a container that is not ready does not count": {
            let p = { metadata: {}, status: { phase: "Running", containerStatuses: [{ ready: true } { ready: false }] } }
            assert (not (any_pod_ready [$p]))
        }
        "a pod still initialising does not count": {
            let p = { metadata: {}, status: { phase: "Pending" } }
            assert (not (any_pod_ready [$p]))
        }
        "cassini is waited on before anything after it is applied": {
            let gate = (gate_after "cassini.yaml")
            assert ($gate != null)
            # the pods' own label: cassini is a Deployment's pod (docs/architecture/deployments.md, step 4)
            assert equal $gate.ref "name=cassini"
        }
        "a manifest that is nobody's dependency has no gate": {
            assert equal (gate_after "kube-observer.yaml") null
        }
    }

    let results = ($tests | columns | each {|name|
        try {
            do ($tests | get $name)
            { name: $name, error: null }
        } catch {|e|
            { name: $name, error: $e.msg }
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
