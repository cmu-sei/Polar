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

# infra/tests/render-determinism.nu
#
# Does rendering a target give the same manifests every time, whoever renders and wherever? A render that
# is to be committed to a deployment repository, and rolled back with `git revert`, has to. This renders the
# target three times with the render cache off:
#
#   first and again   the same conditions, so the pipeline itself is deterministic
#   other environment a different user, timezone, locale and CI variable, and no KUBECONFIG
#
# and compares the three output directories byte for byte, and the per-layer split of each (infra/layers.nu). It then looks in the output for the checkout's
# path, the home directory and the user's name, none of which belong in a manifest. The cache is a separate
# question (it must not change what is rendered); `POLAR_CACHE_DIR=... nu infra/tests/render-determinism.nu --with-cache`
# also renders with a cold and then a warm cache and compares those.
#
# It runs the real render, so it needs what `up.nu` needs to render: the agents' images in the registry and a cluster
# to ask them on (KUBECONFIG). Their scaffolds are collected once, before the first render, and every render reads that
# directory, so the render without KUBECONFIG reads the same answers. It takes a few minutes. It replaces the target's
# output directory (`manifests/`), as any render does.
#
#   nu infra/tests/render-determinism.nu [target] [--with-cache]

use ../layers.nu [split_into]

def render [target: string, name: string, env_vars: record, hide: list<string>, work: string, cache: bool] {
    let base = { POLAR_SCAFFOLD_DIR: ($work | path join "scaffolds") }
    let vars = (if $cache { $base } else { $base | merge { POLAR_CACHE: "off" } })
    let ran = (with-env ($vars | merge $env_vars) {
        for v in $hide { hide-env --ignore-errors $v }
        do { ^nu infra/render.nu $target } | complete
    })
    if $ran.exit_code != 0 {
        error make --unspanned { msg: $"the render '($name)' failed:\n($ran.stderr | lines | last 8 | str join "\n")" }
    }
    let out = (nu $"infra/targets/($target)/target.nu" | from nuon | get output_dir)
    let dest = ($work | path join $name)
    ^cp -a $out $dest
    $dest
}

def differences [a: string, b: string] {
    let r = (do { ^diff -r -q $a $b } | complete)
    if $r.exit_code == 0 { [] } else { $r.stdout | lines }
}

def main [
    target: string = "local-minimal"
    --with-cache            # also compare a cold-cache and a warm-cache render with the uncached one
] {
    let root = (^git rev-parse --show-toplevel | str trim)
    cd $root
    let work = (^mktemp -d | str trim)
    mut problems = []

    let collected = (do { ^nu infra/resolve.nu collect-scaffolds ($work | path join "scaffolds") --target $target } | complete)
    if $collected.exit_code != 0 {
        error make --unspanned { msg: $"the agents could not be asked for their scaffolds \(this needs a cluster, KUBECONFIG, and their images in the registry\):\n($collected.stderr | lines | last 8 | str join "\n")" }
    }

    let first = (render $target "first" {} [] $work false)
    let again = (render $target "again" {} [] $work false)
    let other = (render $target "other-environment" {
        USER: "somebody-else", LOGNAME: "somebody-else", TZ: "Asia/Tokyo",
        LC_ALL: "C", LANG: "C", CI_COMMIT_SHORT_SHA: "deadbeef"
    } ["KUBECONFIG"] $work false)

    for pair in [["again" $again] ["other environment" $other]] {
        let d = (differences $first ($pair | get 1))
        if ($d | is-empty) {
            print $"  ok    ($pair | get 0): identical to the first render"
        } else {
            print -e $"  FAIL  ($pair | get 0) differs:"
            for line in ($d | first 10) { print -e $"          ($line)" }
            $problems = ($problems | append $"($pair | get 0) differs")
        }
    }

    # The layers a render is split into are part of what is committed, so they must come out the same too.
    let layers = (nu $"infra/targets/($target)/target.nu" | from nuon | get -o apply_layers | default [])
    if ($layers | is-not-empty) {
        for pair in [["first" $first] ["again" $again] ["other environment" $other]] {
            split_into $layers ($pair | get 1) ($work | path join $"split-($pair | get 0 | str replace ' ' '-')")
        }
        for name in ["again" "other-environment"] {
            let d = (differences ($work | path join "split-first") ($work | path join $"split-($name)"))
            if ($d | is-empty) {
                print $"  ok    the split of the ($name) render is identical to the first split"
            } else {
                print -e $"  FAIL  the split of the ($name) render differs:"
                for line in ($d | first 10) { print -e $"          ($line)" }
                $problems = ($problems | append $"the split of the ($name) render differs")
            }
        }
    }

    if $with_cache {
        let cache_dir = ($work | path join "cache")
        let cold = (render $target "cold-cache" { POLAR_CACHE_DIR: $cache_dir } [] $work true)
        let warm = (render $target "warm-cache" { POLAR_CACHE_DIR: $cache_dir } [] $work true)
        for pair in [["a cold cache" $cold] ["a warm cache" $warm]] {
            let d = (differences $first ($pair | get 1))
            if ($d | is-empty) {
                print $"  ok    rendering with ($pair | get 0) gives the uncached result"
            } else {
                print -e $"  FAIL  rendering with ($pair | get 0) differs:"
                for line in ($d | first 10) { print -e $"          ($line)" }
                $problems = ($problems | append $"($pair | get 0) differs")
            }
        }
    }

    # What does not belong in a manifest: where the checkout is, whose home it is, who rendered it.
    let user = ($env.USER? | default "")
    let personal = ([$root $env.HOME] ++ (if ($user | str length) >= 4 { [$user] } else { [] }) | uniq)
    for needle in $personal {
        let hit = (do { ^grep -rIl -F -- $needle $first } | complete)
        if $hit.exit_code == 0 {
            let files = ($hit.stdout | lines | each {|f| $f | path basename } | str join ", ")
            print -e $"  FAIL  the output mentions '($needle)': ($files)"
            $problems = ($problems | append $"the output mentions ($needle)")
        } else {
            print $"  ok    the output does not mention '($needle)'"
        }
    }

    ^rm -rf $work
    if ($problems | is-not-empty) {
        print -e $"($problems | length) problem\(s): ($problems | str join '; ')"
        exit 1
    }
    print "the render is deterministic and carries nothing personal"
}
