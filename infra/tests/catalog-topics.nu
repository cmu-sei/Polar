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

# The catalog-wide topic cross-check (docs/architecture/policy-enforcement.md, "Decisions"): every agent in the catalog,
# built from this workspace, asked for its own scaffold (no cluster), with the scheduler's family expanded for the
# agents the catalog serves; every declared topic must have another side somewhere in the catalog, or say why it waits.
# A render sees only the workloads its target runs, so this is where a mismatch fails. It builds every agent, so it takes
# as long as a build of the workspace's agents.

use std/assert
use ../topics.nu [served, judge]
use ../graph.nu [unowned_reads]

const ROOT = path self ../..

# Every catalog workload, whichever target runs it.
def catalog [] {
    glob ($ROOT | path join "infra/config/workloads/*.nuon") | sort | each {|f| open $f } | flatten
}

# The scaffold of a catalog agent, from its binary built here: the crate is `scaffold_package` (or its image's name), and
# the binary is named for the crate.
def scaffold_of [w: record, served_list: string] {
    let package = ($w.scaffold_package? | default $w.image)
    let features = ($w.scaffold_features? | default "")
    let agents = ($ROOT | path join "src/agents")
    let build = (do {
        cd $agents
        if ($features | is-empty) {
            ^cargo build -q -p $package --bin $package
        } else {
            ^cargo build -q -p $package --bin $package --features $features
        }
    } | complete)
    if $build.exit_code != 0 {
        error make --unspanned { msg: $"($w.name): cargo build -p ($package) failed: ($build.stderr | lines | last 5 | str join '; ')" }
    }
    let target = ($env.CARGO_TARGET_DIR? | default ($agents | path join "target"))
    let r = (with-env { POLAR_SERVED_WORKLOADS: $served_list } { do { ^($target | path join "debug" $package) --scaffold } | complete })
    if $r.exit_code != 0 {
        error make --unspanned { msg: $"($w.name): --scaffold exited ($r.exit_code): ($r.stderr | lines | last 3 | str join '; ')" }
    }
    $r.stdout | from json
}

def main [] {
    # The agents (the `agent` and `run` kinds), and cert-issuer (kind `ca`: an agent with a template of its own, docs/architecture/cert-issuer-isolation.md,
    # step 1c), whose topics are the other side of the credentials agent's record topics.
    let agents = (catalog | where {|w| ($w.kind? | default "agent") in ["agent" "run" "ca"] and ($w.config? | default "env") == "document" })
    let served_list = (served (catalog))
    print $"building and asking ($agents | length) catalog agents for their scaffolds..."
    # One at a time: two builds of one crate with different features would share its binary.
    let scaffolds = ($agents | each {|w| { w: $w, scaffold: (scaffold_of $w $served_list) } })
    let graphs = ($scaffolds | each {|x| { workload: $x.w.name, graph: ($x.scaffold.graph? | default []) } })
    let sets = ($scaffolds | each {|x| { workload: $x.w.name, topics: ($x.scaffold.topics? | default []) } })

    let tests = [
        {
            name: "every catalog agent prints its topics"
            body: {
                for s in $sets {
                    assert ($s.topics | is-not-empty) $"($s.workload) prints no topics"
                }
            }
        }
        {
            name: "the scheduler's family is expanded for every agent the catalog serves, and for nothing else"
            body: {
                let expected = ($served_list | split row "," | each {|w| $"agent.($w).configuration" } | sort)
                for scheduler in ["polar-scheduler" "polar-scheduler-observer"] {
                    let family = ($sets | where workload == $scheduler | first | get topics | where from == "family" | get name | sort)
                    assert equal $family $expected
                }
            }
        }
        {
            name: "the state agent's family is expanded for every agent the catalog serves, and each that reads state hears its own"
            body: {
                let expected = ($served_list | split row "," | each {|w| $"soft-state-replies.($w)" } | sort)
                let family = ($sets | where workload == "soft-state-observer" | first | get topics | where from == "family" | get name | sort)
                assert equal $family $expected
                # docs/architecture/soft-state.md: the soft-state rule gives a reader its own reply topic, and no other.
                let readers = ($graphs | where {|g| $g.graph | any {|e| $e.access == "reads" } } | get workload)
                assert ($readers | is-not-empty) "no catalog agent reads state"
                for s in $sets {
                    let heard = ($s.topics | where access == "subscribe" | get name | where {|t| $t | str starts-with "soft-state-replies." })
                    let own = (if $s.workload in $readers { [$"soft-state-replies.($s.workload)"] } else { [] })
                    assert equal $heard $own $s.workload
                }
            }
        }
        {
            name: "every graph label read in the catalog is owned by some workload"
            body: {
                let unowned = (unowned_reads $graphs | each {|u| $"($u.workload) reads ($u.label)" })
                assert ($unowned | is-empty) $"reads of labels nobody owns: ($unowned | str join '; ')"
            }
        }
        {
            name: "every publish in the catalog says what causes it, or why it publishes of its own accord"
            body: {
                # docs/architecture/authority-model.md, "Decisions": the authority derives the paths from these causes.
                let uncaused = ($sets | each {|s|
                    $s.topics | where access == "publish"
                    | where {|t| ($t.causes? | default [] | is-empty) and ($t.unprompted? == null) }
                    | each {|t| $"($s.workload) ($t.name)" }
                } | flatten)
                assert ($uncaused | is-empty) $"publishes with no cause and no reason: ($uncaused | str join '; ')"
            }
        }
        {
            name: "every declared topic in the catalog has another side, or says why it waits"
            body: {
                let j = (judge $sets)
                for w in $j.waiting { print $"        waits: ($w.workload) ($w.access) ($w.topic): ($w.waits)" }
                let said = ($j.failures | each {|f| $"($f.workload) ($f.access) ($f.topic)" } | str join "; ")
                assert ($j.failures | is-empty) $"one-sided, with no reason: ($said)"
            }
        }
    ]

    let results = ($tests | each {|t|
        try { do $t.body; { name: $t.name, error: null } } catch {|e| { name: $t.name, error: $e.msg } }
    })
    for r in $results {
        if $r.error == null { print $"  ok    ($r.name)" } else { print -e $"  FAIL  ($r.name): ($r.error)" }
    }
    let failed = ($results | where error != null | length)
    if $failed > 0 { print -e $"($failed) of ($results | length) failed"; exit 1 }
    print $"all ($results | length) passed"
}
