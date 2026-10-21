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
# infra/load-harness.nu
#
# Runs the Cassini load harness (src/agents/cassini/test; docs/architecture/policy-enforcement.md, stage 5a) on a
# target that enables it, and compares what runs measured.
#
#   nu infra/load-harness.nu run infra/load-plans/baseline.nuon     # every run of a plan, in turn
#   nu infra/load-harness.nu compare ~/.cache/polar/harness/*.nuon  # runs side by side
#
# A run is a set of Jobs copied from the target's rendered harness Jobs (rendered, never applied: `rendered_only`), one
# per sink and per producer, each reading a copy of its workload's configuration document with the run's HARNESS_*
# values in it, so the identity, image, certificate and topics stay the catalog's. The sinks start first; the producers
# start once every sink has logged that the broker acknowledged its subscription. Each pod logs one result line; the
# script keeps them, with the run, the commit, the broker's image and the nodes, in
# ~/.cache/polar/harness/<plan>-<time>.nuon (they hold only for the machine that made them), and deletes the Jobs.
#
# The pure parts are exported and tested (infra/tests/load-harness.nu); the rest asks the cluster.

const NAMESPACE = "polar"
const RESULT_MARK = "HARNESS LOAD RESULT"
const READY_MARK = "HARNESS LOAD READY"
const RUN_LABEL = "polar.harness/run"
# How many times a run's Job replaces a pod that failed.
export const RETRIES = 2
const ROOT = path self ..

# A plan's defaults: what a run is when the plan does not say.
export const DEFAULTS = {
    size: 256
    rate: 1000
    producers: 1
    sinks: 1
    lanes: 1
    seconds: 20
    max_count: 100000
    max_bytes: 1073741824
    warmup: 100
    window: 1000
    window_bytes: 67108864
    stall_secs: 30
    idle_secs: 30
    first_wait_secs: 300
}

# The runs a plan asks for: each of its `runs`, then every combination of its `grid`'s lists (a list of records sets
# several fields at once), each over the plan's `defaults` over DEFAULTS, numbered `<plan>-<n>`, with its count.
export def expand [plan: record]: nothing -> list<record> {
    let defaults = ($DEFAULTS | merge ($plan.defaults? | default {}))
    let grid = ($plan.grid? | default {})
    let combos = ($grid | transpose axis values | reduce --fold [{}] {|axis, partials|
        $partials | each {|partial|
            $axis.values | each {|v|
                $partial | merge (if ($v | describe | str starts-with "record") { $v } else { {($axis.axis): $v} })
            }
        } | flatten
    })
    let combos = if ($grid | is-empty) { [] } else { $combos }
    ($plan.runs? | default []) | append $combos | enumerate | each {|e|
        let run = ($defaults | merge $e.item)
        $run | merge { id: $"($plan.name)-($e.index)", count: (count_for $run) }
    }
}

# How many messages each producer of `run` sends: its `count` if it gives one; otherwise `seconds` at its rate (or
# `max_count`, as fast as it can go), never more than `max_count` nor more than `max_bytes` of payload, and at least one.
export def count_for [run: record]: nothing -> int {
    if ($run.count? != null) { return $run.count }
    let by_time = if $run.rate > 0 { $run.rate * $run.seconds } else { $run.max_count }
    [([$by_time $run.max_count ($run.max_bytes // ([$run.size 1] | math max))] | math min) 1] | math max
}

# The values a producer or a sink of `run` is given (the variables its scaffold declares), as text.
export def values_for [run: record, role: string, index: int]: nothing -> record {
    let shared = {
        HARNESS_RUN_ID: $run.id
        HARNESS_INDEX: $index
        HARNESS_PRODUCERS: $run.producers
        HARNESS_LANES: $run.lanes
        HARNESS_COUNT: $run.count
    }
    let own = if $role == "producer" {
        {
            HARNESS_MESSAGE_SIZE: $run.size
            HARNESS_RATE: $run.rate
            HARNESS_WINDOW: $run.window
            HARNESS_WINDOW_BYTES: $run.window_bytes
            HARNESS_STALL_SECS: $run.stall_secs
        }
    } else {
        {
            HARNESS_WARMUP: $run.warmup
            HARNESS_IDLE_SECS: $run.idle_secs
            HARNESS_FIRST_WAIT_SECS: $run.first_wait_secs
        }
    }
    $shared | merge $own | transpose name value | each {|r| { name: $r.name value: ($r.value | into string) } }
    | reduce --fold {} {|r, acc| $acc | insert $r.name $r.value }
}

# The configuration document with `values` set: each must be a variable it declares, so a renamed variable fails here,
# not in a pod that would run on its default.
export def with_values [document: record, values: record]: nothing -> record {
    let declared = ($document.sections | each {|s| $s.variables | get name } | flatten)
    let unknown = ($values | columns | where {|n| not ($n in $declared) })
    if ($unknown | is-not-empty) {
        error make { msg: $"the agent declares no ($unknown | str join ', ')" }
    }
    $document | update sections {|doc|
        $doc.sections | each {|section|
            $section | update variables {|s|
                $s.variables | each {|v| if ($v.name in $values) { $v | upsert value ($values | get $v.name) } else { $v } }
            }
        }
    }
}

# A run's Job for one producer or sink: the rendered Job renamed, reading `config_map` instead of its workload's
# configuration, labelled with the run, a failed pod replaced at most twice (a pod whose init could not finish exits 1:
# polar-init's escrow deadline), and kept after it ends until the script has read it and deletes it.
export def job_for [job: record, name: string, config_map: string, run_id: string]: nothing -> record {
    let workload = $job.metadata.name
    let labelled = {|labels| ($labels | default {}) | upsert $RUN_LABEL $run_id }
    $job
    | update metadata {|j| $j.metadata | upsert name $name | upsert labels (do $labelled $j.metadata.labels?) }
    | update spec {|j| $j.spec | upsert backoffLimit $RETRIES | upsert ttlSecondsAfterFinished 3600 }
    | update spec.template.metadata {|j|
        $j.spec.template.metadata | upsert name $name | upsert labels (do $labelled $j.spec.template.metadata.labels?)
    }
    | update spec.template.spec.volumes {|j|
        $j.spec.template.spec.volumes | each {|v|
            if ($v.configMap?.name? == $"($workload)-config") { $v | update configMap.name $config_map } else { $v }
        }
    }
}

# The ConfigMap a run's Job reads: the workload's, renamed, labelled with the run, holding `document`.
export def config_map_for [config_map: record, name: string, document: record, run_id: string]: nothing -> record {
    $config_map
    | update metadata {|c| { name: $name namespace: $c.metadata.namespace labels: { $RUN_LABEL: $run_id } } }
    | update data {|c| $c.data | upsert "config.json" ($document | to json) }
}

# The result lines in a pod's log, as records. The log is coloured, and a line that is not a result is ignored.
export def parse_results [logs: string]: nothing -> list<record> {
    $logs | ansi strip | lines | where {|l| $l | str contains $RESULT_MARK } | each {|l|
        try { $l | split row $RESULT_MARK | last | str trim | from json } catch { null }
    } | compact
}

# Whether a sink's log says the broker acknowledged its subscription.
export def is_ready [logs: string]: nothing -> bool {
    $logs | ansi strip | str contains $READY_MARK
}

# One row per run: what was asked, what the producers reached, what the sinks received, the worst sink's latencies, and
# what was lost or refused.
export def summarise [records: list]: nothing -> list<record> {
    let ms = {|values| if ($values | is-empty) { 0 } else { ($values | math max) / 1000000 | math round --precision 2 } }
    # `math sum` refuses an empty list; a run whose pods reported nothing still has a row.
    let total = {|rows, field| $rows | reduce --fold 0 {|row, acc| $acc + ($row | get $field) } }
    $records | each {|r|
        let sinks = ($r.results | where role == "sink")
        let producers = ($r.results | where role == "producer")
        {
            run: $r.run.id
            size: $r.run.size
            rate: $r.run.rate
            shape: $"($r.run.producers)p ($r.run.sinks)s ($r.run.lanes)l"
            count: $r.run.count
            sent_per_sec: (do $total $producers achieved_rate | math round)
            received_per_sec: (do $total $sinks messages_per_sec | math round)
            mb_per_sec: ((do $total $sinks bytes_per_sec) / 1000000 | math round --precision 1)
            p50_ms: (do $ms ($sinks | each {|s| $s.latency_ns.p50 }))
            p99_ms: (do $ms ($sinks | each {|s| $s.latency_ns.p99 }))
            max_ms: (do $ms ($sinks | each {|s| $s.latency_ns.max }))
            lost: (do $total $sinks lost)
            refused: (do $total $producers refused)
            reported: $"($r.results | length)/($r.run.producers + $r.run.sinks)"
            one_node: $r.one_node
            error: ($r.error? | default "")
        }
    }
}

# ── The cluster ───────────────────────────────────────────────────────────────────────────────────────────────────────

def --wrapped kube [...args: string] {
    let r = (^kubectl -n $NAMESPACE ...$args | complete)
    if $r.exit_code != 0 { error make { msg: $"kubectl ($args | str join ' '): ($r.stderr | str trim)" } }
    $r.stdout
}

def apply [object: record] {
    let r = ($object | to json | ^kubectl apply -f - | complete)
    if $r.exit_code != 0 { error make { msg: $"cannot apply ($object.kind) ($object.metadata.name): ($r.stderr | str trim)" } }
}

# The rendered Job and configuration of each role, from the target's render.
def templates [target: string] {
    let config = (^nu ($ROOT | path join $"infra/targets/($target)/target.nu") | from nuon)
    let of = {|workload|
        let job = ($config.output_dir | path join $"($workload).yaml")
        let resources = ($config.output_dir | path join $"($workload)-resources.yaml")
        if not ($job | path exists) or not ($resources | path exists) {
            error make { msg: $"($workload) is not rendered for ($target): enable it in the target's config.nuon and render" }
        }
        # A file of one document opens as a record, of several as a list.
        let objects = (open $resources | if ($in | describe | str starts-with "record") { [$in] } else { $in })
        let config_map = ($objects | where kind == "ConfigMap" | where metadata.name == $"($workload)-config" | first)
        { job: (open $job), config_map: $config_map, document: ($config_map.data."config.json" | from json) }
    }
    { producer: (do $of "harness-producer"), sink: (do $of "harness-sink") }
}

# Every pod's log of a Job (a failed pod is replaced, so there may be more than one).
def logs_of [job: string, container: string] {
    ^kubectl -n $NAMESPACE logs -l $"job-name=($job)" -c $container --tail=-1 --max-log-requests=10 | complete | get stdout
}

# Whether a Job is over: complete, or failed past its retries.
def finished [job: string] {
    let conditions = (kube get job $job -o json | from json | get -o status.conditions | default [])
    $conditions | any {|c| $c.type in [Complete Failed] and $c.status == "True" }
}

# What a run left, kept before its Jobs are deleted: each pod's state and every container's log.
def keep_failed [run_id: string, parts: list, dir: string] {
    mkdir $dir
    kube get pods -l $"($RUN_LABEL)=($run_id)" -o wide | save --force ($dir | path join "pods.txt")
    for p in $parts {
        for c in ["polar-init" $"harness-($p.role)"] {
            ^kubectl -n $NAMESPACE logs -l $"job-name=($p.name)" -c $c --tail=-1 --prefix | complete | get stdout
            | save --force ($dir | path join $"($p.name)-($c).log")
        }
    }
}

def wait_for [what: string, timeout: duration, done: closure] {
    let deadline = ((date now) + $timeout)
    loop {
        if (do $done) { return }
        if (date now) > $deadline { error make { msg: $"timed out after ($timeout) waiting for ($what)" } }
        sleep 2sec
    }
}

def node_of [job: string] {
    kube get pods -l $"job-name=($job)" -o json | from json | get items | get -o 0.spec.nodeName | default "unknown"
}

def broker_image [] {
    kube get pods -l "job-name=cassini" -o json | from json | get items
    | each {|p| $p.status.containerStatuses? | default [] | get imageID } | flatten | first
}

def remove_run [run_id: string] {
    ^kubectl -n $NAMESPACE delete job,configmap -l $"($RUN_LABEL)=($run_id)" --wait=false | complete | ignore
}

# Runs one run: its sinks, then once they are ready its producers; collects every result line; deletes the Jobs.
def run_one [run: record, templates: record, kept: string] {
    let parts = (
        (0..<$run.sinks | each {|i| { role: "sink", index: $i } })
        | append (0..<$run.producers | each {|i| { role: "producer", index: $i } })
        | each {|p| $p | insert name $"harness-($p.role)-($run.id)-($p.index)" }
    )
    let start = {|p|
        let t = ($templates | get $p.role)
        let document = (with_values $t.document (values_for $run $p.role $p.index))
        apply (config_map_for $t.config_map $"($p.name)-config" $document $run.id)
        apply (job_for $t.job $p.name $"($p.name)-config" $run.id)
    }
    let container = {|p| $"harness-($p.role)" }
    try {
        let sinks = ($parts | where role == "sink")
        $sinks | each {|p| do $start $p } | ignore
        wait_for "every sink to be ready" 10min {||
            $sinks | all {|p| is_ready (logs_of $p.name (do $container $p)) }
        }
        $parts | where role == "producer" | each {|p| do $start $p } | ignore
        let longest = ($run.idle_secs + $run.first_wait_secs + ($run.count // ([$run.rate 1] | math max)) + 300)
        wait_for "every producer and sink to finish" ($longest * 1sec) {|| $parts | all {|p| finished $p.name } }
        let results = ($parts | each {|p| parse_results (logs_of $p.name (do $container $p)) } | flatten)
        let nodes = ($parts | each {|p| node_of $p.name } | uniq)
        remove_run $run.id
        { run: $run, results: $results, nodes: $nodes, one_node: (($nodes | length) == 1) }
    } catch {|e|
        let dir = ($kept | path join $run.id)
        try { keep_failed $run.id $parts $dir } catch {|k| print -e $"  could not keep the failed run's logs: ($k.msg)" }
        remove_run $run.id
        print -e $"  ($run.id) failed: ($e.msg); its pods' logs are in ($dir)"
        { run: $run, results: [], nodes: [], one_node: true, error: $e.msg }
    }
}

# Every run of the plan in `plan_file`, in turn, on `target`; the records are saved after each run.
def "main run" [plan_file: string, --target: string = "local-minimal"] {
    let plan = (open $plan_file)
    let runs = (expand $plan)
    let templates = (templates $target)
    let started = (date now | format date "%Y%m%dT%H%M%S")
    let out = ($env.HOME | path join ".cache/polar/harness" $"($plan.name)-($started).nuon")
    mkdir ($out | path dirname)
    let context = {
        plan: $plan.name
        plan_file: $plan_file
        target: $target
        started: $started
        commit: (^git -C $ROOT rev-parse HEAD | str trim)
        dirty: ((^git -C $ROOT status --porcelain | str trim) != "")
        broker_image: (broker_image)
    }
    mut records = []
    for run in $runs {
        let pace = if $run.rate == 0 { "as fast as the window allows" } else { $"at ($run.rate)/s" }
        print $"($run.id): ($run.producers)p ($run.sinks)s ($run.lanes)l, ($run.count) x ($run.size) bytes ($pace)"
        let record = ((run_one $run $templates ($out | str replace ".nuon" "-failed")) | insert context $context)
        $records = ($records | append $record)
        $records | save --force $out
        if not $record.one_node { print -e "  the pods were on more than one node: latencies compare two clocks" }
    }
    print (summarise $records | table --expand)
    print $"saved ($out)"
}

# The runs in `files` (records `run` saved), side by side.
def "main compare" [...files: string] {
    let records = ($files | each {|f| open $f } | flatten)
    print (summarise $records | table --expand)
}

def main [] {
    print "nu infra/load-harness.nu run <plan.nuon> [--target <target>]   |   nu infra/load-harness.nu compare <records.nuon>..."
}
