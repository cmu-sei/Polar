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

# Reading an image reference and the digest header a registry answers with.

use std/assert
use ../digest.nu [digest_from_headers, manifest_urls, parse_image_ref]

def main [] {
    let tests = [
        {
            name: "a reference is split into the registry, the image path and the tag"
            body: {
                assert equal (parse_image_ref "localhost:5000/cassini:latest") { host: "localhost:5000", name: "cassini", tag: "latest" }
                assert equal (parse_image_ref "10.0.2.2:5000/testpub/polar-polar-init:v1") { host: "10.0.2.2:5000", name: "testpub/polar-polar-init", tag: "v1" }
            }
        }
        {
            name: "a digest reference, or a name with no tag, is not one this can look up"
            body: {
                assert equal (parse_image_ref "localhost:5000/cassini@sha256:abc") null
                assert equal (parse_image_ref "localhost:5000/cassini") null
                assert equal (parse_image_ref "cassini:latest") null
            }
        }
        {
            name: "a manifest is asked for over HTTPS first, then over plain HTTP, which only the local registry speaks"
            body: {
                assert equal (manifest_urls { host: "registry.example.org", name: "team/polar-cassini", tag: "v1" }) [
                    "https://registry.example.org/v2/team/polar-cassini/manifests/v1"
                    "http://registry.example.org/v2/team/polar-cassini/manifests/v1"
                ]
            }
        }
        {
            name: "the digest is read from the header whatever its case, and nothing else is taken for one"
            body: {
                assert equal (digest_from_headers "HTTP/1.1 200 OK\r\nDocker-Content-Digest: sha256:abc123\r\nContent-Length: 5\r\n") "sha256:abc123"
                assert equal (digest_from_headers "HTTP/1.1 200 OK\ndocker-content-digest: sha256:abc123\n") "sha256:abc123"
                assert equal (digest_from_headers "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n") null
                assert equal (digest_from_headers "docker-content-digest: md5:abc\n") null
                assert equal (digest_from_headers "") null
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
