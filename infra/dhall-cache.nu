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

# infra/dhall-cache.nu
#
# Dhall's cache, filled from the one built with this repository (src/flake/dhall-cache.nix) before anything renders, so a render
# fetches nothing. Every remote import the charts use is pinned by hash, and Dhall reads a pinned import from its cache, as
# `1220<hash>`, without looking at the files beneath it; on a cold cache it fetches those (dhall-kubernetes is a thousand files,
# one at a time), which takes many minutes and needs the internet. The dev shell sets POLAR_DHALL_CACHE to the built cache, and
# the dev container carries it at /lib/polar/dhall-cache.

const BUILT_IN = "/lib/polar/dhall-cache"

# The directory holding the built cache: POLAR_DHALL_CACHE, else the dev container's, or null when neither is there.
export def source_dir [from_env: string = ""] {
    [$from_env $BUILT_IN] | where {|d| ($d | is-not-empty) and ($d | path exists) } | get -o 0
}

# Dhall's own cache directory: $XDG_CACHE_HOME/dhall, else ~/.cache/dhall.
export def cache_dir [] {
    ($env.XDG_CACHE_HOME? | default ($env.HOME | path join ".cache")) | path join "dhall"
}

# The entries of `from` that `to` lacks, by name. Pure.
export def missing [from_names: list<string>, to_names: list<string>] {
    $from_names | where {|n| $n not-in $to_names }
}

# Copies into Dhall's cache the built entries it lacks. Returns how many it copied; 0 when there is no built cache.
export def seed [] {
    let from = (source_dir ($env.POLAR_DHALL_CACHE? | default ""))
    if $from == null { return 0 }
    let to = (cache_dir)
    mkdir $to
    let names = (missing (ls $from | get name | path basename) (ls $to | get name | path basename))
    # Written as files of this user's, not copied with the store's read-only mode.
    for n in $names { open --raw ($from | path join $n) | save --raw ($to | path join $n) }
    $names | length
}
