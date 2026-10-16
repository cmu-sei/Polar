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

# How images are named when they are published and imported, and which ones.

use std/assert
use ../images.nu [arch_of, published_ref, published_rows, system_of]

def sample [] {
    [
        { image: "neo4j", tag: "5.26.2", kind: "external" }
        { image: "postgres", tag: "16", kind: "mirror" }
        { image: "cassini", tag: "latest", kind: "nix" }
        { image: "polar-tools", tag: "latest", kind: "nix" }
    ]
}

def main [] {
    let tests = [
        {
            name: "machine types become the architecture names images are tagged with"
            body: {
                assert equal (arch_of "x86_64") "amd64"
                assert equal (arch_of "aarch64") "arm64"
                assert equal (arch_of "arm64") "arm64"
                assert equal (arch_of "amd64") "amd64"
                assert ((try { arch_of "riscv64"; "accepted" } catch {|e| $e.msg }) | str contains "riscv64")
            }
        }
        {
            name: "an architecture names the Nix system that builds for it"
            body: {
                assert equal (system_of "arm64") "aarch64-linux"
                assert equal (system_of "aarch64") "aarch64-linux"
                assert equal (system_of "amd64") "x86_64-linux"
            }
        }
        {
            name: "a published image is <from>/polar-<image>:<tag>-<arch>, with or without a trailing slash"
            body: {
                let row = { image: "cassini", tag: "latest", kind: "nix" }
                assert equal (published_ref "docker.io/acme" $row "arm64") "docker.io/acme/polar-cassini:latest-arm64"
                assert equal (published_ref "docker.io/acme/" $row "x86_64") "docker.io/acme/polar-cassini:latest-amd64"
            }
        }
        {
            name: "only the images this repository builds are published, all or the ones named; an unknown name is an error"
            body: {
                assert equal (published_rows (sample) [] | get image) ["cassini" "polar-tools"]
                assert equal (published_rows (sample) ["polar-tools"] | get image) ["polar-tools"]
                assert ((try { published_rows (sample) ["neo4j"]; "accepted" } catch {|e| $e.msg }) | str contains "not an image this repository builds")
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
