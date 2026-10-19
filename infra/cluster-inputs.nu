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

# infra/cluster-inputs.nu
#
# What belongs to one cluster and exists only after the cluster does: today, the git server's administrators (the identity of
# the first admin account is made by the admin flow on the live cluster). They are inputs to the render like any other, so they
# are written down where the render reads them and recorded in the provenance of what is published; they are not committed,
# because a cluster's identities are not the repository's.
#
#   infra/targets/<target>/cluster-inputs.nuon        gitignored
#   { cluster: "<sha256 of the cluster CA>", workloads: { <name>: { env: [{ name, value }] } } }
#
# `workloads.<name>.env` has the shape of the same key in the target's config.nuon, and replaces an entry of that name there.
# `cluster` ties the file to the cluster it was made for: a file from another cluster is discarded, not extended.

export const ADMINS_WORKLOAD = "polar-git-server"
export const ADMINS_VAR = "POLAR_GIT_SERVER_ADMINS"

export def path_of [infra_root: string, target: string] {
    $infra_root | path join "targets" $target "cluster-inputs.nuon"
}

export def empty [] { { cluster: null, workloads: {} } }

# The file's record, or an empty one when there is none.
export def read_inputs [path: string] {
    if ($path | path exists) { open $path } else { empty }
}

# The env entries these inputs give a workload.
export def env_for [inputs: record, workload: string] {
    $inputs | get -o workloads | default {} | get -o $workload | default {} | get -o env | default []
}

# The inputs as they stand for `cluster`: the same, or empty when they were made for another cluster (or there is no cluster yet).
export def reconcile [inputs: record, cluster: any] {
    if $cluster != null and ($inputs | get -o cluster) == $cluster { $inputs } else { empty }
}

export def admins_of [inputs: record] {
    let found = (env_for $inputs $ADMINS_WORKLOAD | where name == $ADMINS_VAR)
    if ($found | is-empty) { [] } else { $found | first | get value | split row "," | each {|i| $i | str trim } | where {|i| $i != "" } }
}

# The inputs with `identity` among the git server's administrators, for `cluster`. `changed` is false when it already was.
export def with_admin [inputs: record, identity: string, cluster: string] {
    let current = (reconcile $inputs $cluster)
    let held = (admins_of $current)
    if $identity in $held { return { inputs: $current, changed: ($current != $inputs) } }
    let others = (env_for $current $ADMINS_WORKLOAD | where name != $ADMINS_VAR)
    let entries = ($others | append { name: $ADMINS_VAR, value: ($held | append $identity | str join ",") })
    {
        inputs: { cluster: $cluster, workloads: ($current.workloads | upsert $ADMINS_WORKLOAD { env: $entries }) }
        changed: true
    }
}

# A workload's env entries from the target's config (`ov`) without the names the cluster supplies, then the cluster's.
export def env_merged [config_env: list, supplied: list] {
    let names = ($supplied | get name)
    ($config_env | where {|e| $e.name not-in $names }) ++ $supplied
}
