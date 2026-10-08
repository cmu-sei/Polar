#!/usr/bin/env nu
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
        "rebuilt images restart a job even when its manifest is unchanged and it is ready": {
            let v = (job_verdict 0 [{ name: "a", ready: true, done: false }] true)
            assert $v.restart
            assert ($v.reason | str contains "rebuilt")
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
            assert equal $gate.ref "job-name=cassini"
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
