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

# The cache key of a hand-written chart's Dhall render, and that a render read back from the cache is the render.

use std/assert
use ../dhall-render.nu [dhall_yaml, env_names_in, key_parts]
use ../cache.nu [content_key]

def main [] {
    let tests = [
        {
            name: "the key changes with the text, the sources, the tool's version and the flags, and with nothing else"
            body: {
                let base = (content_key (key_parts "t" "s" "v1" ["--documents"]))
                assert equal $base (content_key (key_parts "t" "s" "v1" ["--documents"]))
                assert ($base != (content_key (key_parts "t2" "s" "v1" ["--documents"])))
                assert ($base != (content_key (key_parts "t" "s2" "v1" ["--documents"])))
                assert ($base != (content_key (key_parts "t" "s" "v2" ["--documents"])))
                assert ($base != (content_key (key_parts "t" "s" "v1" ["--documents" "--quoted"])))
                assert ($base != (content_key (key_parts "t" "s" "v1" [])))
            }
        }
        {
            name: "a render comes back identical from the cache, and a changed file is rendered again"
            body: {
                let dir = (mktemp -d)
                let file = ($dir | path join "a.dhall")
                with-env { POLAR_CACHE_DIR: ($dir | path join "cache") } {
                    "{ a = 1 }" | save $file
                    let first = (dhall_yaml $file)
                    assert ($first | str contains "a: 1")
                    # the same file, from the cache: even with the file gone, nothing recomputed would be the same text
                    assert equal (dhall_yaml $file) $first
                    "{ a = 2 }" | save --force $file
                    assert ((dhall_yaml $file) | str contains "a: 2")
                    # flags are part of it
                    "[ { a = 1 }, { a = 2 } ]" | save --force $file
                    assert ((dhall_yaml $file ["--documents"]) != (dhall_yaml $file))
                    # an error is an error, not an empty file
                    "{ a = " | save --force $file
                    let failed = (try { dhall_yaml $file; "no error" } catch {|e| $e.msg })
                    assert ($failed | str contains "dhall-to-yaml failed")
                }
                rm -rf $dir
            }
        }
        {
            name: "the environment variables a Dhall file imports are found, once each; a file that imports none adds none"
            body: {
                assert equal (env_names_in ["let r = (env:POLAR_NODE_REGISTRY as Text) ++ \"/\"" "env:B as Text ? env:A as Text" "{ a = 1 }"]) ["A" "B" "POLAR_NODE_REGISTRY"]
                assert equal (env_names_in ["{ a = 1 }"]) []
            }
        }
        {
            name: "a render from the cache is not served when the environment a file imports has changed"
            body: {
                let dir = (mktemp -d)
                let file = ($dir | path join "r.dhall")
                with-env { POLAR_CACHE_DIR: ($dir | path join "cache") } {
                    # the key looks at the repository's own Dhall files, so this checks the render itself: same file, different value, different text
                    "{ registry = env:POLAR_NODE_REGISTRY as Text }" | save $file
                    let a = (with-env { POLAR_NODE_REGISTRY: "10.0.2.2:5000" } { dhall_yaml $file })
                    let b = (with-env { POLAR_NODE_REGISTRY: "polar-registry:5000" } { dhall_yaml $file })
                    assert ($a | str contains "10.0.2.2:5000")
                    assert ($b | str contains "polar-registry:5000")
                }
                rm -rf $dir
            }
        }
    ]

    let results = ($tests | each {|t|
        let r = (try { do $t.body; null } catch {|e| $e.msg })
        { name: $t.name, error: $r }
    })
    for r in $results {
        if $r.error == null { print $"  ok    ($r.name)" } else { print -e $"  FAIL  ($r.name): ($r.error)" }
    }
    let failed = ($results | where error != null | length)
    if $failed > 0 {
        print -e $"($failed) of ($results | length) failed"
        exit 1
    }
    print $"all ($results | length) passed"
}
