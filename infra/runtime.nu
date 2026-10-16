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

# infra/runtime.nu
#
# Which container runtime the scripts use: podman or docker. Polar does not provide either; one
# of them must be installed. The scripts use `POLAR_CONTAINER_RUNTIME` when it is set, and
# otherwise the first of podman and docker they find on the PATH.
#
# The development node of the maintainers' own local cluster is a podman container, so the steps that
# reach into it (`podman exec u7s ...`) are podman-only and run only for POLAR_CLUSTER_RUNTIME=u7s.

const SUPPORTED = ["podman" "docker"]

# The runtime to use, given what the user asked for (an empty string for "no preference") and which
# of the supported runtimes are installed. Pure: `container_runtime` supplies both from the environment.
export def choose [wanted: string, installed: list<string>] {
    if ($wanted | is-not-empty) {
        if $wanted not-in $SUPPORTED {
            error make --unspanned { msg: $"POLAR_CONTAINER_RUNTIME is '($wanted)'; it must be one of: ($SUPPORTED | str join ', ')" }
        }
        if $wanted not-in $installed {
            error make --unspanned { msg: $"POLAR_CONTAINER_RUNTIME is '($wanted)', but ($wanted) is not on your PATH" }
        }
        return $wanted
    }
    let found = ($SUPPORTED | where {|r| $r in $installed })
    if ($found | is-empty) {
        error make --unspanned { msg: "Polar needs a container runtime and found none: install podman or docker, or set POLAR_CONTAINER_RUNTIME" }
    }
    $found | first
}

# The runtime for this machine.
export def container_runtime [] {
    let installed = ($SUPPORTED | where {|r| which $r | is-not-empty })
    choose ($env.POLAR_CONTAINER_RUNTIME? | default "") $installed
}

# `--entrypoint` for running `command` (an image's entrypoint and cmd, as a list). Podman takes the
# whole list as JSON. Docker takes one executable, so the rest travel as arguments after the image.
# Returns the value for --entrypoint and the arguments that must come right after the image name.
export def entrypoint_for [runtime: string, command: list<string>] {
    if $runtime == "docker" {
        { entrypoint: ($command | first), leading: ($command | skip 1) }
    } else {
        { entrypoint: ($command | to json --raw), leading: [] }
    }
}

# Extra flags for pulling from a plain-HTTP local registry. Podman wants it said; docker takes
# localhost registries over HTTP by default and has no such flag.
export def insecure_pull_flags [runtime: string, flags: list<string>] {
    if $runtime == "docker" { [] } else { $flags }
}
