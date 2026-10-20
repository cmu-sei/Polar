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
use ../images.nu [arch_of, external_source, import_rows, ours, published_ref, published_rows, split_ref, system_of]

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
            name: "an image built elsewhere comes from its archive, else is left when the registry has it, else is copied from where it is published"
            body: {
                assert equal (external_source true false "docker.io/acct") "archive"
                assert equal (external_source true true "") "archive"
                assert equal (external_source false true "docker.io/acct") "registry"
                assert equal (external_source false false "docker.io/acct") "published"
                assert equal (external_source false false "") "missing"
            }
        }
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
            name: "by default only the images this repository builds are published; a mirror only when named; an unknown name is an error"
            body: {
                assert equal (published_rows (sample) [] | get image) ["cassini" "polar-tools"]
                assert equal (published_rows (sample) ["polar-tools"] | get image) ["polar-tools"]
                assert equal (published_rows (sample) ["postgres"] | get image) ["postgres"]
                assert equal (published_rows (sample) ["postgres" "cassini"] | get image) ["postgres" "cassini"]
                assert ((try { published_rows (sample) ["neo4j"]; "accepted" } catch {|e| $e.msg }) | str contains "not an image this repository builds")
            }
        }
        {
            name: "a reference is its image's name and tag, whether or not it is pinned by digest"
            body: {
                assert equal (split_ref "localhost:30500/cassini:latest") { ref: "localhost:30500/cassini:latest", name: "cassini", tag: "latest" }
                let pinned = "localhost:30500/neo4j:5.26.2@sha256:30e755f597ff0810f7e4359b4d5ceb49872906a759d6c4ccf35ed50b14a59d7f"
                assert equal (split_ref $pinned) { ref: $pinned, name: "neo4j", tag: "5.26.2" }
                assert equal (split_ref "10.0.2.2:5000/kube-observer") { ref: "10.0.2.2:5000/kube-observer", name: "kube-observer", tag: "latest" }
            }
        }
        {
            name: "only the images under the node registry are the ones to build and push; one from upstream is not"
            body: {
                let refs = ["localhost:30500/cassini:latest" "docker.io/rancher/local-path-provisioner:v0.0.35@sha256:abc" "localhost:30500/neo4j:5.26.2" "localhost:305000/x:y"]
                assert equal ($refs | ours "localhost:30500") ["localhost:30500/cassini:latest" "localhost:30500/neo4j:5.26.2"]
            }
        }
        {
            name: "an import takes what is published here and Neo4j, which is published from its own repository; named, only those"
            body: {
                assert equal (import_rows (sample) [] | get image) ["cassini" "polar-tools" "neo4j"]
                assert equal (import_rows (sample) ["neo4j"] | get image) ["neo4j"]
                assert equal (import_rows (sample) ["cassini" "neo4j"] | get image) ["cassini" "neo4j"]
                assert equal (import_rows (sample) ["cassini"] | get image) ["cassini"]
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
