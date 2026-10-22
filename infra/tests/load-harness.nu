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
# infra/tests/load-harness.nu
#
#   nu infra/tests/load-harness.nu
#
# The parts of infra/load-harness.nu that need no cluster. One test builds the harness agents, to check that every value
# a run gives is a variable they declare.

use std/assert
use ../load-harness.nu *

const ROOT = path self ../..

def sink_result [index: int, lost: int, p99: int, rate: float] {
    {
        role: "sink", run: "p-0", index: $index, topic: "cassini.harness.lane.0", producers: [0], expected: 10,
        received: (10 - $lost), lost: $lost, duplicates: 0, out_of_order: 0, unexpected: 0, warmup: 1,
        latency_ns: { count: 9, p50: 1000000, p90: 2000000, p99: $p99, max: ($p99 * 2), mean: 1500000.0 },
        span_secs: 1.0, messages_per_sec: $rate, bytes_per_sec: ($rate * 256), ended: "complete"
    }
}

def producer_result [index: int, refused: int] {
    {
        role: "producer", run: "p-0", index: $index, topic: "cassini.harness.lane.0", size: 256, rate: 100, count: 10,
        window: 1000, sent: 10, acknowledged: (10 - $refused), refused: $refused, failed: 0, elapsed_secs: 0.1,
        achieved_rate: 100.0
    }
}

# The scaffold of `package`'s binary, which is named for the package unless `binary` says otherwise.
def scaffold_of [package: string, binary?: string] {
    let built = (do { cd ($ROOT | path join "src/agents"); ^cargo build -q -p $package } | complete)
    if $built.exit_code != 0 { error make { msg: $"cannot build ($package): ($built.stderr | lines | last 5 | str join "\n")" } }
    ^($ROOT | path join $"src/agents/target/debug/($binary | default $package)") --scaffold | from json
}

def main [] {
    let tests = [
        {
            name: "a plan's runs come first, then every combination of its grid, each over the defaults, numbered"
            body: {
                let runs = (expand {
                    name: "p"
                    defaults: { warmup: 7 }
                    runs: [{ size: 9, count: 3 }]
                    grid: { size: [1 2], shape: [{ producers: 1, sinks: 1, lanes: 1 } { producers: 4, sinks: 2, lanes: 4 }] }
                })
                assert equal ($runs | get id) [p-0 p-1 p-2 p-3 p-4]
                assert equal ($runs | get size) [9 1 1 2 2]
                assert equal ($runs | get producers) [1 1 4 1 4]
                assert equal ($runs | get sinks) [1 1 2 1 2]
                assert equal ($runs | get warmup | uniq) [7]
                assert equal ($runs | get rate | uniq) [$DEFAULTS.rate]
                assert equal ($runs.0.count) 3
                assert equal (expand { name: "e" }) []
            }
        }
        {
            name: "a plan's broker settings are each a pass of every run, in turn, numbered on; without any, one pass"
            body: {
                let runs = (expand {
                    name: "p"
                    grid: { size: [1 2] }
                    broker: { CASSINI_DECISION_CACHE_SECS: [0 5] }
                })
                assert equal ($runs | get id) [p-0 p-1 p-2 p-3]
                assert equal ($runs | get size) [1 2 1 2]
                assert equal ($runs | get broker) [
                    { CASSINI_DECISION_CACHE_SECS: "0" } { CASSINI_DECISION_CACHE_SECS: "0" }
                    { CASSINI_DECISION_CACHE_SECS: "5" } { CASSINI_DECISION_CACHE_SECS: "5" }
                ]
                assert equal (expand { name: "q", runs: [{}] } | get broker) [{}]
            }
        }
        {
            name: "the broker's settings are variables its scaffold declares, so a rename fails before the broker restarts"
            body: {
                let document = (with_values (scaffold_of "cassini-broker" "cassini-server") { CASSINI_DECISION_CACHE_SECS: "0" })
                let set = ($document.sections | each {|s| $s.variables } | flatten | where name == "CASSINI_DECISION_CACHE_SECS")
                assert equal ($set | get value) ["0"]
            }
        }
        {
            name: "a run's count is its seconds at its rate, capped by the most messages and the most bytes, and at least one"
            body: {
                let run = ($DEFAULTS | merge { seconds: 10, max_count: 5000, max_bytes: 1000000 })
                assert equal (count_for ($run | merge { rate: 100, size: 10 })) 1000
                assert equal (count_for ($run | merge { rate: 10000, size: 10 })) 5000
                assert equal (count_for ($run | merge { rate: 0, size: 10 })) 5000
                assert equal (count_for ($run | merge { rate: 100, size: 10000 })) 100
                assert equal (count_for ($run | merge { rate: 100, size: 10000000 })) 1
                assert equal (count_for ($run | merge { count: 42 })) 42
            }
        }
        {
            name: "a producer and a sink are each given the run and their own variables, as text"
            body: {
                let run = ((expand { name: "p", runs: [{ producers: 3, lanes: 2 }] }) | first)
                let producer = (values_for $run "producer" 2)
                assert equal $producer.HARNESS_RUN_ID "p-0"
                assert equal $producer.HARNESS_INDEX "2"
                assert equal $producer.HARNESS_PRODUCERS "3"
                assert equal $producer.HARNESS_LANES "2"
                assert equal $producer.HARNESS_RATE ($DEFAULTS.rate | into string)
                assert ("HARNESS_WARMUP" not-in ($producer | columns))
                let sink = (values_for $run "sink" 0)
                assert equal $sink.HARNESS_WARMUP ($DEFAULTS.warmup | into string)
                assert ("HARNESS_RATE" not-in ($sink | columns))
            }
        }
        {
            name: "every value a run gives is a variable the agents declare, so a rename fails before any pod runs"
            body: {
                let run = ((expand { name: "p", runs: [{}] }) | first)
                for role in [producer sink] {
                    let document = (with_values (scaffold_of $"harness-($role)") (values_for $run $role 0))
                    let set = ($document.sections | each {|s| $s.variables } | flatten | where name == "HARNESS_RUN_ID")
                    assert equal ($set | get value) ["p-0"]
                }
                let refused = (try { with_values (scaffold_of "harness-sink") { HARNESS_RATE: "1" }; null } catch {|e| $e.msg })
                assert ($refused | str contains "HARNESS_RATE")
            }
        }
        {
            name: "a run's Job is the rendered one renamed, reading its own configuration, labelled, never retried"
            body: {
                let job = {
                    kind: "Job"
                    metadata: { name: "harness-sink", namespace: "polar" }
                    spec: {
                        backoffLimit: 1000000
                        ttlSecondsAfterFinished: 300
                        template: {
                            metadata: { name: "harness-sink", labels: { name: "harness-sink" } }
                            spec: { volumes: [
                                { name: "polar-config", configMap: { name: "harness-sink-config" } }
                                { name: "polar-health", emptyDir: {} }
                            ] }
                        }
                    }
                }
                let j = (job_for $job "harness-sink-p-0-1" "harness-sink-p-0-1-config" "p-0")
                assert equal $j.metadata.name "harness-sink-p-0-1"
                assert equal ($j.metadata.labels | get "polar.harness/run") "p-0"
                assert equal ($j.spec.template.metadata.labels | get "polar.harness/run") "p-0"
                assert equal $j.spec.template.metadata.labels.name "harness-sink"
                assert equal $j.spec.backoffLimit $RETRIES
                assert equal $j.spec.ttlSecondsAfterFinished 3600
                assert equal $j.spec.template.spec.volumes.0.configMap.name "harness-sink-p-0-1-config"
                assert equal $j.spec.template.spec.volumes.1 { name: "polar-health", emptyDir: {} }
                let cm = (config_map_for { kind: "ConfigMap", metadata: { name: "harness-sink-config", namespace: "polar" }, data: { "config.json": "{}" } } "c" { a: 1 } "p-0")
                assert equal $cm.metadata { name: "c", namespace: "polar", labels: { "polar.harness/run": "p-0" } }
                assert equal ($cm.data."config.json" | from json) { a: 1 }
            }
        }
        {
            name: "result lines are read out of a coloured log with other lines in it; a broken one is skipped"
            body: {
                let esc = (char --integer 27)
                let line = {|r| $"($esc)[2m2026-10-21T20:00:00Z($esc)[0m  INFO harness_sink: HARNESS LOAD RESULT ($r | to json --raw)" }
                let logs = ([
                    "noise"
                    (do $line (sink_result 0 0 3000000 10.0))
                    "HARNESS LOAD RESULT {not json"
                    (do $line (producer_result 0 0))
                    $"HARNESS LOAD READY run=p-0 index=0 topic=cassini.harness.lane.0"
                ] | str join "\n")
                let r = (parse_results $logs)
                assert equal ($r | get role) [sink producer]
                assert equal $r.0.latency_ns.p99 3000000
                assert (is_ready $logs)
                assert (not (is_ready "noise"))
            }
        }
        {
            name: "a summary sums the rates, takes the worst sink's latencies, and counts what was lost and refused"
            body: {
                let run = ((expand { name: "p", runs: [{ producers: 2, sinks: 2, lanes: 1 }] }) | first)
                let record = {
                    run: $run
                    results: [(sink_result 0 1 3000000 10.0) (sink_result 1 2 5000000 20.0) (producer_result 0 1) (producer_result 1 0)]
                    nodes: ["n1"]
                    one_node: true
                }
                let s = (summarise [$record] | first)
                assert equal $s.shape "2p 2s 1l"
                assert equal $s.received_per_sec 30
                assert equal $s.sent_per_sec 200
                assert equal $s.p99_ms 5.0
                assert equal $s.max_ms 10.0
                assert equal $s.lost 3
                assert equal $s.refused 1
                assert equal $s.reported "4/4"
                assert equal $s.broker ""
                assert equal $s.error ""
                let cached = (summarise [($record | update run {|r| $r.run | upsert broker { CASSINI_DECISION_CACHE_SECS: "0" } })] | first)
                assert equal $cached.broker "CASSINI_DECISION_CACHE_SECS=0"
                let failed = (summarise [($record | update results [] | insert error "timed out")] | first)
                assert equal [$failed.p99_ms $failed.lost $failed.reported $failed.error] [0 0 "0/4" "timed out"]
            }
        }
    ]

    let results = ($tests | each {|t|
        let r = (try { do $t.body; null } catch {|e| $e.msg })
        { name: $t.name, error: $r }
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
