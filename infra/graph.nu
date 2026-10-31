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

# infra/graph.nu
#
# The catalog's graph schema, as the agents state it in their scaffolds (`graph`: each label with its access, `owns` or
# `reads`, and its purpose; polar-agent-builder/src/graph_schema.rs). Nothing here computes a schema: the agent does.
# What is decided here (docs/architecture/soft-state.md): the body of the state agent's first `schema` section, which
# infra/seed.nu signs into polar-schedules; the reads of a label that no workload owns, which fail the check as a
# one-sided topic does; and that a target running an agent that needs the state agent runs it too.

# The labels a scaffold's `graph` gives `access`, each once, in order.
def labels_of [graph: list, access: string]: nothing -> list {
    $graph | where access == $access | get label | uniq | sort
}

# The reads among `sets` ({ workload, graph }) of a label that nothing in `sets` owns: { workload, label } each. The
# state agent would answer them from nodes nobody writes.
export def unowned_reads [sets: list]: nothing -> list {
    let owned = ($sets | each {|s| labels_of ($s.graph? | default []) "owns" } | flatten | uniq)
    $sets | each {|s|
        labels_of ($s.graph? | default []) "reads"
        | where {|l| $l not-in $owned }
        | each {|l| { workload: $s.workload, label: $l } }
    } | flatten
}

# The body of the state agent's `schema` runtime section (soft-state/observe/src/floor.rs): every workload of `records`
# ({ who, namespace, service_account, graph }) that declares a label, by its one identity
# `<workload>.<namespace>.serviceaccount.<domain>`, with the labels it owns and reads. A workload whose service account is
# not its name would have another identity than its name says, and is an error, as in the authority model (rules.nu
# `model_facts`).
export def state_schema [records: list, domain: string]: nothing -> record {
    let declaring = ($records | where {|r| ($r.graph? | default [] | is-not-empty) })
    let misnamed = ($declaring | where {|r| $r.service_account != $r.who })
    if ($misnamed | is-not-empty) {
        let named = ($misnamed | each {|r| $"($r.who) \(service account ($r.service_account)\)" } | str join ", ")
        error make { msg: $"its service account is not its name, so its identity would name another workload: ($named)" }
    }
    let workloads = ($declaring | each {|r|
        {
            workload: $r.who
            identity: $"($r.who).($r.namespace).serviceaccount.($domain)"
            owns: (labels_of $r.graph "owns")
            reads: (labels_of $r.graph "reads")
        }
    } | sort-by workload)
    { workloads: $workloads }
}

# What is wrong with workload `who`'s needs in a target running `running`: each capability of `caps` whose dependency is
# a workload (soft-state's is the state agent) that the target does not run.
export def needs_workload [who: string, needs: list, caps: record, running: list]: nothing -> list {
    $needs
    | each {|n| { need: $n, workload: ($caps | get -o $n | default {} | get -o dependency.workload) } }
    | where {|d| $d.workload != null and $d.workload not-in $running }
    | each {|d| $"($who) needs ($d.need), which needs the workload ($d.workload), and this target does not run it" }
}
