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

# Dhall's cache is filled from the built one: what it lacks is copied, what it has is left, and with no built cache nothing happens.

use std/assert
use ../dhall-cache.nu [cache_dir, missing, seed, source_dir]

def main [] {
    let tests = [
        {
            name: "only the entries the cache lacks are copied"
            body: {
                assert equal (missing ["1220aa" "1220bb"] ["1220bb" "1220cc"]) ["1220aa"]
                assert equal (missing ["1220aa"] ["1220aa"]) []
            }
        }
        {
            name: "the built cache is POLAR_DHALL_CACHE when it exists, and there is none when nothing exists"
            body: {
                let d = (mktemp --directory)
                assert equal (source_dir $d) $d
                if not ("/lib/polar/dhall-cache" | path exists) {
                    assert equal (source_dir ($d | path join "absent")) null
                    assert equal (source_dir "") null
                }
                rm -rf $d
            }
        }
        {
            name: "seeding copies the missing entries as writable files, keeps what is there, and a second seed copies nothing"
            body: {
                let built = (mktemp --directory)
                let home = (mktemp --directory)
                "new" | save ($built | path join "1220aa")
                "built" | save ($built | path join "1220bb")
                ^chmod 444 ($built | path join "1220aa") ($built | path join "1220bb")
                mkdir ($home | path join "dhall")
                "kept" | save ($home | path join "dhall" "1220bb")
                with-env { POLAR_DHALL_CACHE: $built, XDG_CACHE_HOME: $home } {
                    assert equal (cache_dir) ($home | path join "dhall")
                    assert equal (seed) 1
                    assert equal (open --raw ($home | path join "dhall" "1220aa")) "new"
                    assert equal (open --raw ($home | path join "dhall" "1220bb")) "kept"
                    "rewritten" | save --force ($home | path join "dhall" "1220aa")
                    assert equal (seed) 0
                }
                ^chmod -R u+w $built
                rm -rf $built $home
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
