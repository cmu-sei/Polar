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

# The registry the nodes of `target` name ("" for no particular target: the environment, then the default).
export def node_registry [target: string = ""] {
    let cfg = if $target == "" { null } else {
        let p = ($INFRA | path join "targets" $target "config.nuon")
        if ($p | path exists) { open $p | get -o cluster.registry } else { null }
    }
    pick_registry ($env.POLAR_NODE_REGISTRY? | default "") $cfg (open ($INFRA | path join "config" "cluster.nuon") | get registry)
}

# The host part of a registry address ("10.0.2.2:5000" -> "10.0.2.2").
export def host_of [registry: string] {
    $registry | split row "/" | first | split row ":" | first
}
