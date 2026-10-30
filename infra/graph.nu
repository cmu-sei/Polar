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
# What is decided here is who the state agent answers (docs/architecture/soft-state.md): the workloads that read, which
# its reply family is expanded over; its floor, written into its document; and the reads of a label that no workload
# owns, which fail the check as a one-sided topic does.

# The variable a family over the readers is expanded from, as a scaffold names it in a family's `over`.
export const READERS_VARIABLE = "POLAR_STATE_READERS"

# The labels a scaffold's `graph` gives `access`, each once, in order.
def labels_of [graph: list, access: string]: nothing -> list {
    $graph | where access == $access | get label | uniq | sort
}

# The workloads among `sets` ({ workload, graph }) that read a label, comma-separated as POLAR_STATE_READERS takes them.
export def readers [sets: list]: nothing -> string {
    $sets
    | where {|s| ($s.graph? | default [] | where access == "reads" | is-not-empty) }
    | get workload
    | uniq
    | sort
    | str join ","
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

# The state agent's floor (soft-state/observe/src/floor.rs): every workload of `records` ({ who, namespace,
# service_account, graph }) that declares a label, by its one identity `<workload>.<namespace>.serviceaccount.<domain>`,
# with the labels it owns and reads. A workload whose service account is not its name would have another identity than
# its name says, and is an error, as in the authority model (rules.nu `model_facts`).
export def state_floor [records: list, domain: string]: nothing -> record {
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
    { format: 1, workloads: $workloads }
}

# Whether a scaffold has a topic family expanded over the readers.
export def over_readers [scaffold: record]: nothing -> bool {
    $scaffold.topic_families? | default [] | any {|f| ($f.over? | default "") == $READERS_VARIABLE }
}
