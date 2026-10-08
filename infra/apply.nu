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
# more than 1 it could not compare. `jobs` is each Job in the file with whether its pod is
# ready and whether it has finished. `rebuilt` is whether the images were just rebuilt: they
# keep their tags, so an unchanged manifest would leave the old code running.
export def job_verdict [diff_exit: int, jobs: list, rebuilt: bool = false] {
    if $rebuilt {
        return { restart: true, reason: "the images were rebuilt (--dev), and keep their tags" }
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
