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

# infra/tests/cache.nu
#
# The cache render-catalog.nu keeps for what Dhall renders. A manifest is a pure function of its inputs, so one
# already rendered from the same inputs is read back instead of rendered again: that is most of the time a
# resume takes. What matters is that it can never serve a manifest the inputs no longer produce.
#
#   nu infra/tests/cache.nu

use std/assert
use ../cache.nu *

# A fresh cache directory for one test, and a counter file the compute closure appends to.
def fresh [] {
    let dir = (mktemp --directory)
    { dir: $dir, counter: ($dir | path join "calls.txt") }
}

def calls [f: record] {
    if ($f.counter | path exists) { open --raw $f.counter | lines | length } else { 0 }
}

def main [] {
    let tests = [
        {
            name: "the second ask for the same key is read back, not computed again"
            body: {
                let f = (fresh)
                with-env { POLAR_CACHE_DIR: ($f.dir | path join "cache") } {
                    let compute = {|| "one\n" | save --append $f.counter; "rendered text" }
                    assert equal (cached_text "render" "k1" $compute) "rendered text"
                    assert equal (cached_text "render" "k1" $compute) "rendered text"
                }
                assert equal (calls $f) 1
                rm -rf $f.dir
            }
        }
        {
            name: "a different key is computed on its own"
            body: {
                let f = (fresh)
                with-env { POLAR_CACHE_DIR: ($f.dir | path join "cache") } {
                    let compute = {|| "one\n" | save --append $f.counter; "x" }
                    cached_text "render" "k1" $compute | ignore
                    cached_text "render" "k2" $compute | ignore
                }
                assert equal (calls $f) 2
                rm -rf $f.dir
            }
        }
        {
            name: "a failure is not cached, and the error reaches the caller"
            body: {
                let f = (fresh)
                with-env { POLAR_CACHE_DIR: ($f.dir | path join "cache") } {
                    let failing = {|| error make { msg: "dhall said no" } }
                    let r = (try { cached_text "render" "k1" $failing; "no error" } catch {|e| $e.msg })
                    assert ($r | str contains "dhall said no")
                    # asked again, it is computed again (nothing was stored), and this time it can succeed
                    assert equal (cached_text "render" "k1" {|| "fine" }) "fine"
                }
                rm -rf $f.dir
            }
        }
        {
            name: "POLAR_CACHE=off computes every time and stores nothing"
            body: {
                let f = (fresh)
                with-env { POLAR_CACHE_DIR: ($f.dir | path join "cache"), POLAR_CACHE: "off" } {
                    let compute = {|| "one\n" | save --append $f.counter; "x" }
                    cached_text "render" "k1" $compute | ignore
                    cached_text "render" "k1" $compute | ignore
                }
                assert equal (calls $f) 2
                assert (not ($f.dir | path join "cache" | path exists))
                rm -rf $f.dir
            }
        }
        {
            name: "nothing half-written is left behind, so a reader never sees a partial manifest"
            body: {
                let f = (fresh)
                with-env { POLAR_CACHE_DIR: ($f.dir | path join "cache") } {
                    cached_text "render" "k1" {|| "text" } | ignore
                }
                let files = (ls ($f.dir | path join "cache" "render") | get name | each {|p| $p | path basename })
                assert equal $files ["k1.txt"]
                rm -rf $f.dir
            }
        }
        {
            name: "the cache directory is the one asked for, else under XDG_CACHE_HOME, else under HOME"
            body: {
                with-env { POLAR_CACHE_DIR: "/x/y" } { assert equal (cache_dir) "/x/y" }
                with-env { POLAR_CACHE_DIR: null, XDG_CACHE_HOME: "/xdg" } { assert equal (cache_dir) "/xdg/polar" }
                with-env { POLAR_CACHE_DIR: null, XDG_CACHE_HOME: null, HOME: "/home/u" } { assert equal (cache_dir) "/home/u/.cache/polar" }
            }
        }
        {
            name: "a key is the same for the same parts and differs when any part does, and the parts cannot run together"
            body: {
                assert equal (content_key ["a" "b"]) (content_key ["a" "b"])
                assert ((content_key ["a" "b"]) != (content_key ["a" "c"]))
                assert ((content_key ["a" "b"]) != (content_key ["ab" ""]))
                assert ((content_key ["ab"]) != (content_key ["a" "b"]))
            }
        }
        {
            name: "a directory's digest changes when a file's content changes, when one is added, and when one is renamed"
            body: {
                let d = (mktemp --directory)
                "let a = 1\nin a" | save ($d | path join "a.dhall")
                "let b = 2\nin b" | save ($d | path join "b.dhall")
                let base = (dir_digest $d "*.dhall")
                assert equal (dir_digest $d "*.dhall") $base
                "let a = 9\nin a" | save --force ($d | path join "a.dhall")
                let edited = (dir_digest $d "*.dhall")
                assert ($edited != $base)
                "let c = 3\nin c" | save ($d | path join "c.dhall")
                assert ((dir_digest $d "*.dhall") != $edited)
                rm ($d | path join "c.dhall")
                mv ($d | path join "b.dhall") ($d | path join "z.dhall")
                assert ((dir_digest $d "*.dhall") != $edited)
                # a file that does not match the pattern is not part of it
                "ignored" | save ($d | path join "notes.txt")
                let before = (dir_digest $d "*.dhall")
                "ignored, edited" | save --force ($d | path join "notes.txt")
                assert equal (dir_digest $d "*.dhall") $before
                rm -rf $d
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
