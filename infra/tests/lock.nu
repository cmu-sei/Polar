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

# The image lock: pinning a manifest's images by digest, and keeping the lock itself.

use std/assert
use ../lock.nu [committed_what, format_lock, lay_over, lock_problems, lookup, pin_text, upsert]

const REG = "10.0.2.2:5000"
const D1 = "sha256:1111111111111111111111111111111111111111111111111111111111111111"
const D2 = "sha256:2222222222222222222222222222222222222222222222222222222222222222"
const D3 = "sha256:3333333333333333333333333333333333333333333333333333333333333333"

def entry [image: string, tag: string, arch: string, digest: string] {
    { image: $image, tag: $tag, arch: $arch, digest: $digest }
}

def main [] {
    let tests = [
        {
            name: "the committed lock says the commit a release built it from, and only when it was built"
            body: {
                assert equal (committed_what) "What a release is made of: the digest each image has in the registry."
                assert equal (committed_what "") (committed_what)
                assert ((committed_what "abc123") | str ends-with "Built from commit abc123.")
                # What it says is a comment: the entries read back the same.
                let rows = [(entry "a" "latest" "amd64" $D1)]
                assert equal ((format_lock $rows (committed_what "abc123")) | from nuon) $rows
            }
        }
        {
            name: "an image line is pinned in the plain and in the quoted spelling, and nothing else changes"
            body: {
                let lock = [(entry "kube" "latest" "amd64" $D1) (entry "init" "v2" "amd64" $D2)]
                let text = $"        image: ($REG)/kube:latest\n  - 'image': \"($REG)/init:v2\"\n        imagePullPolicy: IfNotPresent\n    name: kube\n"
                let r = (pin_text $text $lock $REG "amd64")
                assert equal $r.pinned 2
                assert equal $r.missing []
                assert equal $r.text $"        image: ($REG)/kube:latest@($D1)\n  - 'image': \"($REG)/init:v2@($D2)\"\n        imagePullPolicy: IfNotPresent\n    name: kube\n"
            }
        }
        {
            name: "pinning twice gives what pinning once did"
            body: {
                let lock = [(entry "kube" "latest" "amd64" $D1)]
                let once = (pin_text $"image: ($REG)/kube:latest\n" $lock $REG "amd64")
                let twice = (pin_text $once.text $lock $REG "amd64")
                assert equal $twice.text $once.text
                assert equal $twice.pinned 0
            }
        }
        {
            name: "an image with no entry is reported, left as it was, and the others are still pinned"
            body: {
                let lock = [(entry "kube" "latest" "amd64" $D1)]
                let text = $"image: ($REG)/kube:latest\nimage: ($REG)/other:latest\n"
                let r = (pin_text $text $lock $REG "amd64")
                assert equal $r.missing ["other:latest"]
                assert ($r.text | str contains $"($REG)/other:latest\n") "the unlocked line is untouched"
                assert ($r.text | str contains $"($REG)/kube:latest@($D1)")
            }
        }
        {
            name: "the entry is chosen by tag as well as name, so a tag that moved is not silently pinned to the old one"
            body: {
                let lock = [(entry "init" "v1" "amd64" $D1)]
                let r = (pin_text $"image: ($REG)/init:v2\n" $lock $REG "amd64")
                assert equal $r.missing ["init:v2"]
            }
        }
        {
            name: "the architecture picks the digest: amd64 and arm64 are different images"
            body: {
                let lock = [(entry "kube" "latest" "amd64" $D1) (entry "kube" "latest" "arm64" $D2)]
                assert equal (lookup $lock "kube" "latest" "arm64") $D2
                assert equal (pin_text $"image: ($REG)/kube:latest\n" $lock $REG "arm64" | get text) $"image: ($REG)/kube:latest@($D2)\n"
                assert equal (lookup $lock "kube" "latest" "riscv64") null
            }
        }
        {
            name: "an image from another registry, and an untagged one, are handled by the registry they name"
            body: {
                let lock = [(entry "kube" "latest" "amd64" $D1)]
                let r = (pin_text $"image: docker.io/library/busybox:1\nimage: ($REG)/kube\n" $lock $REG "amd64")
                assert equal $r.missing []
                assert ($r.text | str contains "docker.io/library/busybox:1\n") "another registry's image is not ours to pin"
                assert ($r.text | str contains $"($REG)/kube@($D1)") "no tag means latest"
            }
        }
        {
            name: "an upsert replaces the entry for the same image, tag and architecture and keeps the rest"
            body: {
                let rows = [(entry "a" "1" "amd64" $D1) (entry "a" "1" "arm64" $D2)]
                let after = (upsert $rows (entry "a" "1" "amd64" $D3))
                assert equal ($after | length) 2
                assert equal (lookup $after "a" "1" "amd64") $D3
                assert equal (lookup $after "a" "1" "arm64") $D2
            }
        }
        {
            name: "this machine's lock lies over the committed one entry by entry"
            body: {
                let committed = [(entry "a" "1" "amd64" $D1) (entry "b" "1" "amd64" $D1)]
                let dev = [(entry "a" "1" "amd64" $D2)]
                let merged = (lay_over $committed $dev)
                assert equal (lookup $merged "a" "1" "amd64") $D2
                assert equal (lookup $merged "b" "1" "amd64") $D1
            }
        }
        {
            name: "a lock is written sorted, one entry a line, with the banner, and the same every time"
            body: {
                let rows = [(entry "b" "1" "amd64" $D1) (entry "a" "2" "amd64" $D2) (entry "a" "1" "amd64" $D3)]
                let text = (format_lock $rows "What this is.")
                assert equal $text (format_lock ($rows | reverse) "What this is.")
                assert ($text | str contains "DM26-0670") "the banner is there"
                let entries = ($text | lines | where {|l| $l | str starts-with "  {image:" })
                assert equal ($entries | length) 3
                assert ($entries | first | str contains "image: \"a\", tag: \"1\"") "sorted by image, then tag"
                let back = ($text | from nuon)
                assert equal ($back | length) 3
                assert equal (lookup $back "a" "2" "amd64") $D2
            }
        }
        {
            name: "a digest that is not sha256 and 64 hex, and an entry twice, are problems"
            body: {
                let p = (lock_problems [(entry "a" "1" "amd64" "sha256:short") (entry "b" "1" "amd64" $D1) (entry "b" "1" "amd64" $D2)])
                assert ($p | any {|x| $x | str contains "a:1 (amd64) has a digest that is not"})
                assert ($p | any {|x| $x | str contains "b:1 amd64 is in the lock more than once"})
                assert equal (lock_problems [(entry "a" "1" "amd64" $D1)]) []
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
