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

# infra/layers.nu
#
# A target's manifests, grouped into layers. A layer is a set of rendered files that is applied together, after
# the layers it depends on are healthy. The target declares them (`apply_layers` in target.nu), and its flat
# `apply_order` is derived from them, so `up.nu` applies exactly what it always did. A layer is also the unit
# a GitOps tool reconciles on its own: Flux gives each one a Kustomization that `dependsOn` the layers named
# here and waits for the workloads listed.
#
# A layer is a record:
#   name        unique, and the name of its directory in a split render
#   files       the rendered files, in the order they are applied
#   depends_on  names of layers that come before it; nothing in a layer may need what a later one makes
#   wait_for    what must be up before a layer that depends on this one starts: a list of
#               { kind: "pods" | "statefulset", ns, ref, label }, as `up.nu` waits for them
#
#   nu infra/layers.nu check <target>          the layers cover what the target renders, and nothing twice
#   nu infra/layers.nu split <target> <dest>   copy the render into <dest>/<layer>/ with a layers.json

use rules.nu [apply_order_problems]

const WAIT_KINDS = ["pods" "statefulset"]

# The flat order the layers add up to.
export def apply_order_of [layers: list] {
    $layers | each {|l| $l.files } | flatten
}

# What is wrong with the layers, given the files the render made.
export def layer_problems [layers: list, rendered: list<string>] {
    mut problems = []

    let names = ($layers | get name)
    let repeated = ($names | uniq --repeated)
    for n in $repeated { $problems = ($problems | append $"the layer name '($n)' is used more than once") }

    for l in ($layers | enumerate) {
        let layer = $l.item
        let before = ($names | first $l.index)
        if ($layer.files | is-empty) {
            $problems = ($problems | append $"layer '($layer.name)' has no files")
        }
        for dep in $layer.depends_on {
            if $dep not-in $names {
                $problems = ($problems | append $"layer '($layer.name)' depends on '($dep)', which is not a layer")
            } else if $dep not-in $before {
                $problems = ($problems | append $"layer '($layer.name)' depends on '($dep)', which does not come before it")
            }
        }
        for w in $layer.wait_for {
            if ($w.kind? | default "") not-in $WAIT_KINDS {
                $problems = ($problems | append $"layer '($layer.name)' waits for something of kind '($w.kind? | default '')'; it must be one of: ($WAIT_KINDS | str join ', ')")
            }
            for field in [ns ref label] {
                if ($w | get -o $field | default "" | is-empty) {
                    $problems = ($problems | append $"layer '($layer.name)' has a wait_for with no ($field)")
                }
            }
        }
    }

    let all = (apply_order_of $layers)
    for f in ($all | uniq --repeated) {
        let where = ($layers | where {|l| $f in $l.files } | get name | str join ', ')
        $problems = ($problems | append $"($f) is in more than one place in the layers: ($where)")
    }
    for f in ($rendered | where {|f| $f not-in $all }) {
        $problems = ($problems | append $"($f) is rendered but is in no layer, so nothing would apply it")
    }
    # The rule about a Job's -resources file coming first holds across the whole order.
    $problems | append (apply_order_problems $rendered $all)
}

# The layout of a split render: for each layer, its directory, files and relations.
export def layout [layers: list] {
    $layers | each {|l|
        { name: $l.name, dir: $l.name, depends_on: $l.depends_on, wait_for: $l.wait_for, files: $l.files }
    }
}

# Copies the files of the render in `src` into one directory per layer under `dest`, in layer order, and
# writes layers.json beside them. Nothing is changed in `src`.
export def split_into [layers: list, src: string, dest: string] {
    mkdir $dest
    for l in (layout $layers) {
        let dir = ($dest | path join $l.dir)
        mkdir $dir
        for f in $l.files {
            let from = ($src | path join $f)
            if ($from | path exists) { ^cp $from ($dir | path join $f) }
        }
    }
    layout $layers | to json --indent 2 | save --force ($dest | path join "layers.json")
}

def load_target [target: string] {
    let file = ($env.PWD | path join $"infra/targets/($target)/target.nu")
    if not ($file | path exists) { error make --unspanned { msg: $"no such target: ($target)" } }
    let config = (nu $file | from nuon)
    if ($config.apply_layers? | default [] | is-empty) {
        error make --unspanned { msg: $"the target '($target)' declares no apply_layers" }
    }
    $config
}

# What of a render is applied: every rendered file but those the target renders and never applies (`rendered_only`: the
# load harness's Jobs, which infra/load-harness.nu copies per run). Those belong to no layer.
export def to_apply [rendered: list<string>, rendered_only: list<string>]: nothing -> list<string> {
    $rendered | where {|f| $f not-in $rendered_only }
}

def rendered_in [config: record] {
    let dir = $config.output_dir
    if not ($dir | path exists) { error make --unspanned { msg: $"nothing rendered at ($dir): run nu infra/render.nu first" } }
    to_apply (glob ($dir | path join "*.yaml") | each {|f| $f | path basename } | sort) ($config.rendered_only? | default [])
}

def "main check" [target: string] {
    let config = (load_target $target)
    let problems = (layer_problems $config.apply_layers (rendered_in $config))
    if ($problems | is-not-empty) {
        for p in $problems { print -e $"  ERROR: ($p)" }
        exit 1
    }
    print $"ok: ($config.apply_layers | length) layers cover the ($config.apply_order | length) files of ($target)"
}

def "main split" [target: string, dest: string] {
    let config = (load_target $target)
    let problems = (layer_problems $config.apply_layers (rendered_in $config))
    if ($problems | is-not-empty) {
        for p in $problems { print -e $"  ERROR: ($p)" }
        exit 1
    }
    split_into $config.apply_layers $config.output_dir ($dest | path expand)
    print $"split ($config.output_dir) into ($config.apply_layers | length) layers under ($dest | path expand)"
}

def main [] {
    print "usage: nu infra/layers.nu check <target> | split <target> <dest>"
}
