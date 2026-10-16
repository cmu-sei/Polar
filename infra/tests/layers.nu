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

# A target's layers: what they add up to, what is wrong with a set of them, and how a render is split.

use std/assert
use ../layers.nu [apply_order_of, layer_problems, layout, split_into]

def layer [name: string, files: list<string>, depends_on: list<string> = [], wait_for: list = []] {
    { name: $name, files: $files, depends_on: $depends_on, wait_for: $wait_for }
}

def has [problems: list<string>, needle: string] {
    assert ($problems | any {|p| $p | str contains $needle }) $"expected a problem containing '($needle)', got: ($problems)"
}

def main [] {
    let tests = [
        {
            name: "the flat order is the layers' files, layer by layer"
            body: {
                let layers = [(layer "a" ["1.yaml" "2.yaml"]) (layer "b" ["3.yaml"] ["a"])]
                assert equal (apply_order_of $layers) ["1.yaml" "2.yaml" "3.yaml"]
            }
        }
        {
            name: "sound layers that cover the render have no problems"
            body: {
                let layers = [(layer "a" ["1.yaml"]) (layer "b" ["2.yaml"] ["a"] [{ kind: "pods", ns: "polar", ref: "name=x", label: "x" }])]
                assert equal (layer_problems $layers ["1.yaml" "2.yaml"]) []
            }
        }
        {
            name: "a rendered file in no layer is reported, because nothing would apply it"
            body: { has (layer_problems [(layer "a" ["1.yaml"])] ["1.yaml" "2.yaml"]) "2.yaml is rendered but is in no layer" }
        }
        {
            name: "a file in two layers is reported with both names"
            body: {
                let p = (layer_problems [(layer "a" ["1.yaml"]) (layer "b" ["1.yaml"] ["a"])] ["1.yaml"])
                has $p "1.yaml is in more than one place"
                has $p "a, b"
            }
        }
        {
            name: "a layer with no files, and a name used twice, are reported"
            body: {
                has (layer_problems [(layer "a" [])] []) "layer 'a' has no files"
                has (layer_problems [(layer "a" ["1.yaml"]) (layer "a" ["2.yaml"])] ["1.yaml" "2.yaml"]) "'a' is used more than once"
            }
        }
        {
            name: "a dependency must be a layer, and one that comes earlier"
            body: {
                has (layer_problems [(layer "a" ["1.yaml"] ["nowhere"])] ["1.yaml"]) "depends on 'nowhere', which is not a layer"
                has (layer_problems [(layer "a" ["1.yaml"] ["b"]) (layer "b" ["2.yaml"])] ["1.yaml" "2.yaml"]) "which does not come before it"
            }
        }
        {
            name: "something waited for must have a known kind and say what it is"
            body: {
                let bad = [(layer "a" ["1.yaml"] [] [{ kind: "cronjob", ns: "polar", ref: "x", label: "x" }])]
                has (layer_problems $bad ["1.yaml"]) "of kind 'cronjob'"
                let blank = [(layer "a" ["1.yaml"] [] [{ kind: "pods", ns: "polar", ref: "", label: "x" }])]
                has (layer_problems $blank ["1.yaml"]) "wait_for with no ref"
            }
        }
        {
            name: "a Job's -resources file after the Job is still caught, across layers"
            body: {
                let layers = [(layer "a" ["job.yaml"]) (layer "b" ["job-resources.yaml"] ["a"])]
                has (layer_problems $layers ["job.yaml" "job-resources.yaml"]) "job-resources.yaml is applied after job.yaml"
            }
        }
        {
            name: "splitting copies every file into its layer's directory and loses nothing"
            body: {
                let work = (^mktemp -d | str trim)
                let src = ($work | path join "src")
                mkdir $src
                for f in ["1.yaml" "2.yaml" "3.yaml"] { $"content of ($f)" | save ($src | path join $f) }
                let layers = [(layer "a" ["1.yaml" "2.yaml"]) (layer "b" ["3.yaml"] ["a"])]
                split_into $layers $src ($work | path join "out")
                assert equal (open ($work | path join "out/a/1.yaml")) "content of 1.yaml"
                assert equal (open ($work | path join "out/b/3.yaml")) "content of 3.yaml"
                let copied = (glob ($work | path join "out/*/*.yaml") | each {|f| $f | path basename } | sort)
                assert equal $copied ["1.yaml" "2.yaml" "3.yaml"]
                let described = (open ($work | path join "out/layers.json"))
                assert equal ($described | get name) ["a" "b"]
                assert equal ($described | get 1.depends_on) ["a"]
                assert equal (glob ($src | path join "*.yaml") | length) 3 "the source is untouched"
                ^rm -rf $work
            }
        }
        {
            name: "splitting twice gives the same bytes"
            body: {
                let work = (^mktemp -d | str trim)
                let src = ($work | path join "src")
                mkdir $src
                for f in ["1.yaml" "2.yaml"] { $"content of ($f)" | save ($src | path join $f) }
                let layers = [(layer "a" ["1.yaml"]) (layer "b" ["2.yaml"] ["a"])]
                split_into $layers $src ($work | path join "one")
                split_into $layers $src ($work | path join "two")
                assert equal (do { ^diff -r ($work | path join "one") ($work | path join "two") } | complete | get exit_code) 0
                ^rm -rf $work
            }
        }
    ]

    let results = ($tests | each {|t|
        try { do $t.body; { name: $t.name, error: null } } catch {|e| { name: $t.name, error: $e.msg } }
    })
    for r in $results {
        if $r.error == null { print $"  ok    ($r.name)" } else { print -e $"  FAIL  ($r.name): ($r.error)" }
    }
    let failed = ($results | where error != null | length)
    if $failed > 0 { print -e $"($failed) of ($results | length) failed"; exit 1 }
    print $"all ($results | length) passed"
}
