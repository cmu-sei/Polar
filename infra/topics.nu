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

# infra/topics.nu
#
# The catalog's topics, as the agents state them in their scaffolds (`topics`: each with its access and where it came
# from, `declared`, `family` or `implied`, and, for a declared one, why it waits). Nothing here computes a set: the
# agent does (docs/architecture/topics.md). What is decided here is which workloads a family serves, and which
# declared topics have no other side (docs/architecture/policy-enforcement.md, "Decisions").

# The workloads a topic family is expanded for, comma-separated as POLAR_SERVED_WORKLOADS takes them: the agents among
# `workloads` (catalog entries) that read a configuration document. A service (the broker) is not a Cassini client and
# subscribes to no configuration topic.
export def served [workloads: list]: nothing -> string {
    $workloads
    | where {|w| ($w.kind? | default "agent") == "agent" and ($w.config? | default "env") == "document" }
    | get name
    | sort
    | str join ","
}

# The declared and family topics of `sets` ({ workload, topics }) that nothing in `sets` is on the other side of: no
# publish of a topic subscribed to, or no subscription to one published. Any topic counts as another side, an implied
# one included; an implied topic is never itself one-sided, since it follows from what the agent is.
export def one_sided [sets: list]: nothing -> list {
    let all = ($sets | each {|s| $s.topics | each {|t| { name: $t.name, access: $t.access } } } | flatten)
    let published = ($all | where access == "publish" | get name | uniq)
    let subscribed = ($all | where access == "subscribe" | get name | uniq)
    $sets | each {|s|
        $s.topics
        | where {|t| $t.from != "implied" }
        | where {|t| if $t.access == "publish" { $t.name not-in $subscribed } else { $t.name not-in $published } }
        | each {|t| { workload: $s.workload, access: $t.access, topic: $t.name, waits: ($t.waits? | default null) } }
    } | flatten
}

# The one-sided topics of `sets`, judged: those that give no reason are `failures`, and those that say why they wait
# are `waiting`.
export def judge [sets: list]: nothing -> record {
    let found = (one_sided $sets)
    { failures: ($found | where waits == null), waiting: ($found | where waits != null) }
}

# The workloads whose certificates would carry more than `limit` topic names (declared and family; implied ones are not
# written), each with how many. Decision C of docs/architecture/policy-enforcement.md: a warning, as a signal to look at
# the agent's design, not a failure; a certificate's hard limit is far above it (measured: about 560 such names fit the
# JDK's 32 KiB handshake message, which Neo4j's TLS enforces, and about 1,100 rustls' 64 KiB).
export def oversized [sets: list, limit: int]: nothing -> list {
    $sets
    | each {|s| { workload: $s.workload, carried: ($s.topics | where {|t| $t.from != "implied" } | length) } }
    | where carried > $limit
}
