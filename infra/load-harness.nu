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
# script keeps them, with the run, the commit, the broker's image, the nodes and the broker's CPU and memory over the run
# (from its node's kubelet), in
# ~/.cache/polar/harness/<plan>-<time>.nuon (they hold only for the machine that made them), and deletes the Jobs.
#
# The pure parts are exported and tested (infra/tests/load-harness.nu); the rest asks the cluster.

const NAMESPACE = "polar"
const RESULT_MARK = "HARNESS LOAD RESULT"
const READY_MARK = "HARNESS LOAD READY"
const RUN_LABEL = "polar.harness/run"
# The broker's pods: a Deployment's, labelled with its name (docs/architecture/deployments.md, step 4).
export const BROKER_SELECTOR = "name=cassini"
const BROKER_CONTAINER = "cassini"
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
    sink_delay_us: 0
    sink_slow: "after_ack"
}

# Every combination of the lists in `axes` (a list of records sets several fields at once); with none, one empty one.
def combinations [axes: record]: nothing -> list<record> {
    $axes | transpose axis values | reduce --fold [{}] {|axis, partials|
        $partials | each {|partial|
            $axis.values | each {|v|
                $partial | merge (if ($v | describe | str starts-with "record") { $v } else { {($axis.axis): $v} })
            }
        } | flatten
    }
}

# The runs a plan asks for: each of its `runs`, then every combination of its `grid`'s lists, each over the plan's
# `defaults` over DEFAULTS, numbered `<plan>-<n>`, with its count. A plan's `broker` lists settings of the broker's
# (variables its configuration document declares) to measure under: every run is made once for each combination of
# them, in turn, with the combination as its `broker`, as text; without any, once, with `broker` empty. A run may give
# settings of its own (`broker: {..}` in a run, or in a record of the grid), over the pass's; the broker is restarted
# with them before it.
export def expand [plan: record]: nothing -> list<record> {
    let defaults = ($DEFAULTS | merge ($plan.defaults? | default {}))
    let grid = ($plan.grid? | default {})
    let combos = if ($grid | is-empty) { [] } else { combinations $grid }
    let as_text = {|settings| $settings | transpose name value | reduce --fold {} {|v, acc| $acc | insert $v.name ($v.value | into string) } }
    let passes = (combinations ($plan.broker? | default {}) | each {|c| do $as_text $c })
    $passes | each {|broker|
        ($plan.runs? | default []) | append $combos | each {|r| $r | upsert broker ($broker | merge (do $as_text ($r.broker? | default {}))) }
    }
    | flatten | enumerate | each {|e|
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
            HARNESS_SINK_DELAY_US: $run.sink_delay_us
            HARNESS_SINK_SLOW: $run.sink_slow
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

# The broker's usage as the kubelet reports it: from its node's summary (`/stats/summary`), the cumulative CPU and the
# memory now of the broker's container in pod `pod`, with when they were sampled (every 10 to 15 seconds); from cAdvisor's
# metrics, the most memory its cgroup has held since the container started. Null when the summary has no such container.
export def broker_usage_of [summary: record, metrics: string, pod: string]: nothing -> any {
    let container = ($summary.pods? | default []
        | where {|p| $p.podRef.namespace == $NAMESPACE and $p.podRef.name == $pod }
        | get -o 0.containers | default [] | where name == $BROKER_CONTAINER | get -o 0)
    if $container == null { return null }
    let peak = ($metrics | lines
        | where {|l| ($l | str starts-with "container_memory_max_usage_bytes{") and ($l | str contains $'container="($BROKER_CONTAINER)"') and ($l | str contains $'pod="($pod)"') }
        | get -o 0 | default "" | split row "} " | get -o 1 | default "" | split row " " | first)
    {
        pod: $pod
        at: ($container.cpu.time | into datetime)
        cpu_ns: $container.cpu.usageCoreNanoSeconds
        working_set: $container.memory.workingSetBytes
        rss: $container.memory.rssBytes
        peak: (if ($peak | is-empty) { null } else { $peak | into float | into int })
    }
}

# What the broker used over one run, from a sample before it and one after: the CPU between them (seconds, and
# microseconds a message received), and its memory at the end, in MB. Null unless both are samples of one container.
export def run_usage [before: any, after: any, received: int]: nothing -> any {
    if $before == null or $after == null { return null }
    if $before.pod != $after.pod or $after.cpu_ns < $before.cpu_ns { return null }
    let cpu_ns = ($after.cpu_ns - $before.cpu_ns)
    let mb = {|b| if $b == null { null } else { $b / 1000000 | math round --precision 1 } }
    {
        cpu_s: ($cpu_ns / 1000000000 | math round --precision 2)
        us_per_msg: (if $received > 0 { $cpu_ns / $received / 1000 | math round --precision 1 } else { null })
        working_set_mb: (do $mb $after.working_set)
        rss_mb: (do $mb $after.rss)
        peak_mb: (do $mb $after.peak)
    }
}

# The broker's log lines a run keeps as events, and what each is.
export const EVENT_MARKS = [
    { kind: "alarm", mark: "the broker's memory budget is exhausted: every reader waits" }
    { kind: "alarm_lifted", mark: "the broker's memory budget is below its low water again" }
    { kind: "spared", mark: "a stalled subscriber's connection waits for room" }
    { kind: "stall_ended", mark: "a subscriber held its topic back past the stall bound" }
    { kind: "session_ended", mark: "a session is ended before its retention window" }
    { kind: "shed", mark: "a subscriber whose client is away lets go of what it held" }
    { kind: "starved", mark: "the memory the broker can have is below its floor" }
]

# What the broker's bounds said over a run, from its log read with `--timestamps` (docs/architecture/cassini-broker.md,
# "The bounds, as built"): each of its memory controller's decisions (`controller`: budget, ceiling, target, limited_by,
# drain_rate, held, alarm, starved; logged when what limits it changes, and every ten samples otherwise), each bounds'
# report (`reports`, every 30 s: used, limit, peak, refusals, slow disconnections, alarms), and each event (`events`:
# the alarm rising and lifting, a stalled subscriber spared or ended, a session ended or shed), each with its time.
export def budget_lines [logs: string]: nothing -> record {
    let lines = ($logs | ansi strip | lines | where {|l| ($l | str contains "the broker's memory budget") or ($EVENT_MARKS | any {|m| $l | str contains $m.mark }) })
    let at_of = {|l| try { $l | split row " " | first | into datetime } catch { null } }
    let fields_of = {|text|
        $text | split row ", " | each {|kv| $kv | parse --regex '^(?P<k>[a-z_]+):(?P<v>.*)$' } | flatten
        | reduce --fold {} {|kv, acc|
            let v = ($kv.v | str trim)
            $acc | insert $kv.k (if ($v =~ '^-?[0-9]+$') { $v | into int } else if $v in ["true" "false"] { $v == "true" } else { $v })
        }
    }
    let marked = "the broker's memory budget, "
    let decisions = ($lines | where {|l| $l | str contains $"($marked)budget:" } | each {|l|
        { at: (do $at_of $l) } | merge (do $fields_of ($l | split row $marked | last))
    })
    let reports = ($lines | where {|l| $l | str contains $"($marked)used:" } | each {|l|
        # Its largest holders are a list with commas in it: kept as written, apart from the rest.
        let body = ($l | split row $marked | last)
        let parts = ($body | split row ", largest:")
        { at: (do $at_of $l) } | merge (do $fields_of ($parts | first)) | insert largest ($parts | get -o 1 | default "")
    })
    let events = ($lines | each {|l|
        let kind = ($EVENT_MARKS | where {|m| $l | str contains $m.mark } | get -o 0.kind)
        if $kind == null { null } else { { at: (do $at_of $l), kind: $kind, line: ($l | split row "] " | last) } }
    } | compact)
    { controller: $decisions, reports: $reports, events: $events }
}

# A run's budget, in brief: the most it reached and held, what limited it (each, in the order seen), and how many of
# each event. Null for a run without the broker's log.
export def budget_brief [budget: any]: nothing -> any {
    if $budget == null { return null }
    let c = $budget.controller
    let most = {|rows, field| if ($rows | is-empty) { null } else { $rows | get $field | math max } }
    {
        budget_max: (do $most $c budget)
        held_max: (do $most $c held)
        target_max: (do $most $c target)
        limited_by: ($c | get -o limited_by | default [] | uniq)
        peak: (do $most $budget.reports peak)
        slow_disconnections: (do $most $budget.reports slow_disconnections)
        events: ($budget.events | group-by kind | transpose kind rows | each {|g| { $g.kind: ($g.rows | length) } } | reduce --fold {} {|e, acc| $acc | merge $e })
    }
}

# A run's Job with its harness container's image at `tag` in place of what the target rendered (a digest, or a tag), so
# rounds of one plan can run one build's harness against that build's broker; the other containers (the custody
# daemon, polar-init) are left as rendered. An empty tag leaves the Job as it is.
export def with_image_tag [job: record, tag: string]: nothing -> record {
    if ($tag | is-empty) { return $job }
    $job | update spec.template.spec.containers {|j|
        $j.spec.template.spec.containers | each {|c|
            if ($c.name | str starts-with "harness-") {
                # `<registry>/<name>:<tag>@sha256:…` or `<registry>/<name>:<tag>`: the name is all before the last ':' of
                # the part before any '@' (a registry may have a port).
                let bare = ($c.image | split row "@" | first)
                let slash = ($bare | split row "/" )
                let last = ($slash | last | split row ":" | first)
                $c | update image ($slash | drop 1 | append $"($last):($tag)" | str join "/")
            } else { $c }
        }
    }
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
            sink: (if ($r.run.sink_delay_us? | default 0) == 0 { "" } else { $"($r.run.sink_delay_us)us ($r.run.sink_slow? | default 'after_ack')" })
            broker: ($r.run.broker? | default {} | transpose name value | each {|v| $"($v.name)=($v.value)" } | str join " ")
            broker_cpu_s: ($r.broker_usage?.cpu_s? | default null)
            broker_us_per_msg: ($r.broker_usage?.us_per_msg? | default null)
            broker_ws_mb: ($r.broker_usage?.working_set_mb? | default null)
            broker_peak_mb: ($r.broker_usage?.peak_mb? | default null)
            budget_max: ($r.budget?.budget_max? | default null)
            held_max: ($r.budget?.held_max? | default null)
            limited_by: ($r.budget?.limited_by? | default [] | str join " ")
            stall_ended: ($r.budget?.events?.stall_ended? | default 0)
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
    kube get pods -l $BROKER_SELECTOR -o json | from json | get items
    | each {|p| $p.status.containerStatuses? | default [] | get imageID } | flatten | first
}

# The broker's usage now, from its node's kubelet (the summary and cAdvisor), or null when it cannot be read: a run goes
# on without it.
def broker_usage [] {
    try {
        let pod = (kube get pods -l $BROKER_SELECTOR -o json | from json | get items
            | where {|p| ($p.status.phase? | default "") == "Running" } | first)
        let base = $"/api/v1/nodes/($pod.spec.nodeName)/proxy"
        let summary = (^kubectl get --raw $"($base)/stats/summary" | from json)
        let metrics = (^kubectl get --raw $"($base)/metrics/cadvisor")
        broker_usage_of $summary $metrics $pod.metadata.name
    } catch { null }
}

# A sample of the broker's usage taken after `since`: the kubelet samples every 10 to 15 seconds, so the last seconds of a
# run are in only a later sample. Null after a minute without one.
def broker_usage_after [since: datetime] {
    let deadline = ((date now) + 1min)
    loop {
        let u = (broker_usage)
        if $u != null and $u.at > $since { return $u }
        if (date now) > $deadline { return null }
        sleep 3sec
    }
}

# Restarts the broker reading `document` in its ConfigMap `config_map`, and waits for it to be ready. Only with Flux
# suspended, as the procedure has it for measuring (docs/architecture/broker-baseline.md), or Flux would put the
# rendered one back; resuming it does, after a run that failed before it could.
def set_broker [config_map: record, document: record] {
    # The broker is polar-cassini's where the target has that kustomization (the first allocator set found it put the
    # rendered broker back mid-run, broker-baseline.md), polar-agents' otherwise.
    for name in [polar-agents polar-cassini] {
        let flux = (^kubectl -n flux-system get kustomization $name -o json | complete)
        if $flux.exit_code != 0 and $name == "polar-cassini" { continue }
        let suspended = if $flux.exit_code == 0 { $flux.stdout | from json | get -o spec.suspend | default false } else { false }
        if not $suspended {
            error make { msg: $"the broker's settings are changed for a run only with Flux's ($name) suspended" }
        }
    }
    let before = (kube get pods -l $BROKER_SELECTOR -o json | from json | get items | get metadata.name)
    kube patch configmap $config_map.metadata.name --type merge -p ({ data: { "config.json": ($document | to json) } } | to json --raw) | ignore
    kube delete pod -l $BROKER_SELECTOR --wait=false | ignore
    wait_for "the broker to come back" 5min {||
        kube get pods -l $BROKER_SELECTOR -o json | from json | get items
        | where {|p| not ($p.metadata.name in $before) }
        | any {|p| $p.status.conditions? | default [] | any {|c| $c.type == "Ready" and $c.status == "True" } }
    }
}

def remove_run [run_id: string] {
    ^kubectl -n $NAMESPACE delete job,configmap -l $"($RUN_LABEL)=($run_id)" --wait=false | complete | ignore
}

# The broker's running pod and its image, now.
def broker_pod [] {
    kube get pods -l $BROKER_SELECTOR -o json | from json | get items
    | where {|p| ($p.status.phase? | default "") == "Running" and $p.metadata.deletionTimestamp? == null }
    | get -o 0 | if $in == null { null } else {
        { name: $in.metadata.name, image: ($in.status.containerStatuses? | default [] | where name == $BROKER_CONTAINER | get -o 0.imageID) }
    }
}

# What the broker's bounds said since `since`, from pod `pod`'s log, or null when it cannot be read.
def broker_budget_since [pod: string, since: datetime] {
    let stamp = ($since | date to-timezone UTC | format date "%Y-%m-%dT%H:%M:%SZ")
    let r = (^kubectl -n $NAMESPACE logs $pod -c $BROKER_CONTAINER --timestamps $"--since-time=($stamp)" | complete)
    if $r.exit_code != 0 { return null }
    budget_lines $r.stdout
}

# Runs one run: its sinks, then once they are ready its producers; collects every result line, the broker's usage and
# what its bounds said over the run; deletes the Jobs.
def run_one [run: record, templates: record, kept: string, image_tag: string = ""] {
    let parts = (
        (0..<$run.sinks | each {|i| { role: "sink", index: $i } })
        | append (0..<$run.producers | each {|i| { role: "producer", index: $i } })
        | each {|p| $p | insert name $"harness-($p.role)-($run.id)-($p.index)" }
    )
    let start = {|p|
        let t = ($templates | get $p.role)
        let document = (with_values $t.document (values_for $run $p.role $p.index))
        apply (config_map_for $t.config_map $"($p.name)-config" $document $run.id)
        apply (with_image_tag (job_for $t.job $p.name $"($p.name)-config" $run.id) $image_tag)
    }
    let container = {|p| $"harness-($p.role)" }
    try {
        let began = (date now)
        let pod = (broker_pod)
        let before = (broker_usage)
        let sinks = ($parts | where role == "sink")
        $sinks | each {|p| do $start $p } | ignore
        wait_for "every sink to be ready" 10min {||
            $sinks | all {|p| is_ready (logs_of $p.name (do $container $p)) }
        }
        $parts | where role == "producer" | each {|p| do $start $p } | ignore
        let longest = ($run.idle_secs + $run.first_wait_secs + ($run.count // ([$run.rate 1] | math max)) + 300)
        wait_for "every producer and sink to finish" ($longest * 1sec) {|| $parts | all {|p| finished $p.name } }
        let ended = (date now)
        let results = ($parts | each {|p| parse_results (logs_of $p.name (do $container $p)) } | flatten)
        let nodes = ($parts | each {|p| node_of $p.name } | uniq)
        remove_run $run.id
        let received = ($results | where role == "sink" | reduce --fold 0 {|r, acc| $acc + $r.received })
        let usage = (run_usage $before (broker_usage_after $ended) $received)
        if $usage == null { print -e "  the broker's usage could not be read for this run" }
        let after = (broker_pod)
        let log = if $pod == null { null } else { broker_budget_since $pod.name $began }
        if $log == null { print -e "  the broker's log could not be read for this run" }
        {
            run: $run, results: $results, nodes: $nodes, one_node: (($nodes | length) == 1), broker_usage: $usage
            broker_pod: $pod, broker_restarted: (($pod | get -o name) != ($after | get -o name))
            broker_log: $log, budget: (budget_brief $log)
        }
    } catch {|e|
        let dir = ($kept | path join $run.id)
        try { keep_failed $run.id $parts $dir } catch {|k| print -e $"  could not keep the failed run's logs: ($k.msg)" }
        remove_run $run.id
        print -e $"  ($run.id) failed: ($e.msg); its pods' logs are in ($dir)"
        { run: $run, results: [], nodes: [], one_node: true, error: $e.msg }
    }
}

# Every run of the plan in `plan_file`, in turn, on `target`; the records are saved after each run. `--image-tag` runs
# the harness's producer and sink from their images under that tag in the target's registry, in place of what the
# target rendered (to run another build's harness against that build's broker).
def "main run" [plan_file: string, --target: string = "local-minimal", --image-tag: string = ""] {
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
        image_tag: $image_tag
    }
    let broker_config = (kube get configmap cassini-config -o json | from json)
    let broker_document = ($broker_config.data."config.json" | from json)
    mut records = []
    mut broker = {}
    # A plan's `fresh_broker: true` restarts the broker before every run, so each begins where its memory budget does
    # (at its floor, with no drain measured), not where the run before it left it.
    let fresh = ($plan.fresh_broker? | default false)
    for run in $runs {
        if $run.broker != $broker or $fresh {
            print $"the broker restarts with ($run.broker | to nuon)"
            set_broker $broker_config (with_values $broker_document $run.broker)
            $broker = $run.broker
        }
        let pace = if $run.rate == 0 { "as fast as the window allows" } else { $"at ($run.rate)/s" }
        print $"($run.id): ($run.producers)p ($run.sinks)s ($run.lanes)l, ($run.count) x ($run.size) bytes ($pace)"
        let record = ((run_one $run $templates ($out | str replace ".nuon" "-failed") $image_tag) | insert context $context)
        $records = ($records | append $record)
        $records | save --force $out
        if not $record.one_node { print -e "  the pods were on more than one node: latencies compare two clocks" }
    }
    if $broker != {} or $fresh {
        print "the broker restarts as the target rendered it"
        set_broker $broker_config $broker_document
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
