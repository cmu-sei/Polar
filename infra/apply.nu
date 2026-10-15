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

# infra/apply.nu
#
# The decisions `up.nu` makes while it applies manifests, kept apart from the kubectl calls so
# they can be tested (infra/tests/apply.nu).
#
#   Jobs     A Job's pod template is immutable, so a changed Job is deleted and applied again.
#            An unchanged one whose pods are ready is left running: restarting it only to
#            re-create what is already there drops every session its peers hold.
#   Gates    Some manifests are what the ones after them connect to. A client that starts
#            before its broker attaches to a broker that is going away, so the next manifest is
#            applied only once the gate's pods are ready.

const POLAR = "polar"

# What must be ready before the manifests after `file` are applied; null if nothing.
export def gate_after [file: string] {
    let hit = (gates | where file == $file)
    if ($hit | is-empty) { null } else { $hit | first }
}

def gates [] {
    [
        { file: "cassini.yaml", ns: $POLAR, ref: "job-name=cassini", label: "cassini" }
    ]
}

# The Jobs in what `kubectl get -f <manifest> -o json` returned: a List for several objects,
# the object itself for one. `done` is whether the Job has run to completion.
export def jobs_in [got: record] {
    let objects = if ($got.kind? | default "") == "List" { $got.items } else { [$got] }
    $objects
    | where {|o| ($o.kind? | default "") == "Job" }
    | each {|o|
        {
            name: $o.metadata.name
            ns: ($o.metadata.namespace? | default $POLAR)
            done: (($o.status?.succeeded? | default 0) > 0)
        }
    }
}

# Whether to delete and re-apply a manifest holding Jobs, and why.
#
# `diff_exit` is the exit code of `kubectl diff -f <manifest>`: 0 no differences, 1 differences,
# more than 1 it could not compare. `jobs` is each Job in the file with whether its pod is ready
# and whether it has finished. `stale` is what `stale_images` found for them: images are rebuilt
# under the same tag, so an unchanged manifest would leave the old code running, and only a Job
# whose own image is no longer what the registry serves has anything to restart for. `changed_config`
# is the ConfigMaps these Jobs use that were just changed: a Job reads its ConfigMap when it starts, so
# an unchanged manifest would leave it running on the old one.
export def job_verdict [diff_exit: int, jobs: list, stale: list<string> = [], changed_config: list<string> = []] {
    if ($stale | is-not-empty) {
        return { restart: true, reason: $"its image was rebuilt, ($stale | str join '; ')" }
    }
    if ($changed_config | is-not-empty) {
        return { restart: true, reason: $"its ConfigMap changed, ($changed_config | str join ', ')" }
    }
    if $diff_exit == 1 {
        return { restart: true, reason: "differs from the cluster (or is not there yet)" }
    }
    if $diff_exit != 0 {
        return { restart: true, reason: "could not compare it with the cluster" }
    }
    let unwell = ($jobs | where {|j| not ($j.ready or $j.done) } | get name)
    if ($unwell | is-empty) {
        { restart: false, reason: "unchanged, and its pods are ready" }
    } else {
        { restart: true, reason: $"unchanged, but not ready: ($unwell | str join ', ')" }
    }
}

# The ConfigMaps `kubectl apply` reported as changed (it says `configured`; `created` has no running
# user yet, and `unchanged` changed nothing).
export def changed_configmaps [apply_output: string] {
    $apply_output
    | lines
    | parse --regex '^configmap/(?<name>\S+) configured\s*$'
    | get name
}

# The workloads in a manifest that read a ConfigMap, and which: `{ kind, name, ns, configmaps }` for each Job,
# Deployment and StatefulSet. They read it when they start, so a changed ConfigMap changes nothing for
# one that is running.
export def config_users [manifest: string] {
    $manifest
    | split row "\n---"
    | each {|text| try { $text | from yaml } catch { null } }
    | compact
    | where {|d| ($d | describe | str starts-with "record") and (($d.kind? | default "") in ["Job" "Deployment" "StatefulSet"]) }
    | each {|d|
        let pod = ($d | get -o spec.template.spec | default {})
        let containers = (($pod | get -o containers | default []) ++ ($pod | get -o initContainers | default []))
        let from_volumes = ($pod | get -o volumes | default [] | each {|v|
            [ ($v | get -o configMap.name) ] ++ ($v | get -o projected.sources | default [] | each {|s| $s | get -o configMap.name })
        } | flatten)
        let from_env_from = ($containers | each {|c| $c | get -o envFrom | default [] | each {|e| $e | get -o configMapRef.name } } | flatten)
        let from_env = ($containers | each {|c| $c | get -o env | default [] | each {|e| $e | get -o valueFrom.configMapKeyRef.name } } | flatten)
        {
            kind: $d.kind
            name: $d.metadata.name
            ns: ($d.metadata?.namespace? | default $POLAR)
            configmaps: (($from_volumes ++ $from_env_from ++ $from_env) | compact | uniq)
        }
    }
}

# Whether a Deployment or StatefulSet that uses ConfigMaps that just changed has to be rolled: it reads them
# when its pods start. Not when its own spec changed (its `metadata.generation` went up, so it is rolling
# already: `kubectl apply` prints `configured` for more than that, so its words are not the test), and not when it
# did not exist before. `before` and `after` are the generation around the apply, null when there is none.
export def workload_needs_restart [before: any, after: any, changed_configmaps: list] {
    ($changed_configmaps | is-not-empty) and ($before != null) and ($before == $after)
}

# The images in `running` (each `{ image, digest }`: what a pod is running) that the registry no longer
# serves under that tag: `served` is `{ image, digest }` for each the registry answered for. An image the
# registry did not answer for is never called stale, since nothing is known. Each is a sentence saying so.
export def stale_images [running: list, served: list] {
    $running
    | uniq
    | each {|r|
        let hit = ($served | where image == $r.image)
        if ($hit | is-empty) { null } else {
            let now = ($hit | first | get digest)
            if $now == $r.digest { null } else { $"($r.image) runs ($r.digest), the registry has ($now)" }
        }
    }
    | compact
}

# Whether any pod in `items` (the `items` of `kubectl get pods -o json`) is Running with every
# container ready. A pod that is being deleted does not count: it still reports ready until it
# is gone, and a wait that counted the old broker would let the clients start before the new one.
export def any_pod_ready [items: list] {
    $items | any {|p|
        let statuses = ($p.status?.containerStatuses? | default [])
        let leaving = (($p.metadata?.deletionTimestamp? | default null) != null)
        let running = (($p.status?.phase? | default "") == "Running")
        let all_ready = ($statuses | is-not-empty) and ($statuses | all {|c| $c.ready })
        (not $leaving) and $running and $all_ready
    }
}
