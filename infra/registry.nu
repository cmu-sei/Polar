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

# infra/registry.nu
#
# The registry as the cluster's nodes name it: the prefix of every image reference in a rendered manifest ("10.0.2.2:5000" for a podman node,
# which reaches the host's registry through slirp4netns's gateway; "polar-registry:5000" or "host.minikube.internal:5000" elsewhere). It is one
# value, resolved here, and everything that needs it asks:
#
#   POLAR_NODE_REGISTRY                      the environment (a tester sets this one and edits nothing in the repository)
#   targets/<target>/config.nuon  cluster.registry   the target's own
#   config/cluster.nuon  registry            the default
#
# Not this machine's way to the same registry (POLAR_REGISTRY, `localhost:5000`: where images are pushed and digests asked), and not the host's
# address for the deployment git server (`POLAR_DEPLOY_HOST`, flux.nu).

const INFRA = (path self | path dirname)

# The first of the three that says something, without a trailing slash. Pure.
export def pick_registry [from_env: string, from_target: any, default: string] {
    let chosen = [$from_env ($from_target | default "") $default] | where {|r| ($r | str trim) != "" } | first
    $chosen | str trim | str trim --right --char "/"
}

# What a target's own cluster settings say about the registry, or null. A target that keeps its registry in the cluster
# (infra/cluster-registry.nu) says `registry_in_cluster: { node_port: N }`, and its nodes then name it `localhost:N`: a NodePort
# answers on every node's localhost, and a node's container runtime pulls from a localhost registry over plain HTTP with no
# configuration. Saying that and an address as well is an error, since the two would disagree. Pure.
export def target_registry [cluster: record] {
    let in_cluster = ($cluster.registry_in_cluster? | default null)
    if $in_cluster == null { return ($cluster.registry? | default null) }
    if ($cluster.registry? | default "" | is-not-empty) {
        error make --unspanned { msg: $"the target's cluster settings give both `registry` \(($cluster.registry)\) and `registry_in_cluster`: keep one" }
    }
    $"localhost:($in_cluster.node_port)"
}

# Where this machine pushes to, and asks digests of, the registry of a target with these cluster settings: for a registry in the
# cluster, the local end of its port-forward (`registry_in_cluster.local_port`, default 5000); otherwise null, which leaves
# POLAR_REGISTRY's default. Pure.
export def target_push_registry [cluster: record] {
    let in_cluster = ($cluster.registry_in_cluster? | default null)
    if $in_cluster == null { null } else { $"localhost:($in_cluster.local_port? | default 5000)" }
}

# The registry this machine pushes to for `target`: POLAR_REGISTRY, then the target's (a registry in its cluster), then localhost:5000.
export def push_registry [target: string] {
    pick_registry ($env.POLAR_REGISTRY? | default "") (target_push_registry (target_cluster $target)) "localhost:5000"
}

# The cluster settings in `targets/<target>/config.nuon`, or an empty record.
export def target_cluster [target: string] {
    let p = ($INFRA | path join "targets" $target "config.nuon")
    if ($p | path exists) { open $p | get -o cluster | default {} } else { {} }
}

# The registry the nodes of `target` name ("" for no particular target: the environment, then the default).
export def node_registry [target: string = ""] {
    let cfg = if $target == "" { null } else { target_registry (target_cluster $target) }
    pick_registry ($env.POLAR_NODE_REGISTRY? | default "") $cfg (open ($INFRA | path join "config" "cluster.nuon") | get registry)
}

# The host part of a registry address ("10.0.2.2:5000" -> "10.0.2.2").
export def host_of [registry: string] {
    $registry | split row "/" | first | split row ":" | first
}
