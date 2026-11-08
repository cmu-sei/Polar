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

# Which container runtime is chosen, and how an image's command line is handed to it.

use std/assert
use ../runtime.nu [choose, entrypoint_for, insecure_pull_flags]

def fails_with [needle: string, body: closure] {
    let r = (try { do $body; "no error" } catch {|e| $e.msg })
    assert ($r | str contains $needle) $"expected an error containing '($needle)', got: ($r)"
}

def main [] {
    let tests = [
        {
            name: "podman is preferred when both are installed and nothing was asked for"
            body: { assert equal (choose "" ["docker" "podman"]) "podman" }
        }
        {
            name: "docker is used when it is the only one installed"
            body: { assert equal (choose "" ["docker"]) "docker" }
        }
        {
            name: "what the user asked for wins over the preference"
            body: { assert equal (choose "docker" ["podman" "docker"]) "docker" }
        }
        {
            name: "asking for a runtime that is not installed is an error that says so"
            body: { fails_with "docker is not on your PATH" { choose "docker" ["podman"] } }
        }
        {
            name: "asking for something that is not podman or docker is an error that lists the choices"
            body: { fails_with "podman, docker" { choose "nerdctl" ["podman" "docker"] } }
        }
        {
            name: "having neither is an error that says what to install"
            body: { fails_with "install podman or docker" { choose "" [] } }
        }
        {
            name: "podman is given an image's whole command line as one JSON list"
            body: {
                let e = (entrypoint_for "podman" ["/bin/agent" "--scaffold"])
                assert equal $e.entrypoint '["/bin/agent","--scaffold"]'
                assert equal $e.leading []
            }
        }
        {
            name: "docker is given the executable, and the rest of the command line after the image"
            body: {
                let e = (entrypoint_for "docker" ["/bin/agent" "--flag" "x"])
                assert equal $e.entrypoint "/bin/agent"
                assert equal $e.leading ["--flag" "x"]
            }
        }
        {
            name: "only podman is told that a local registry is plain HTTP"
            body: {
                assert equal (insecure_pull_flags "podman" ["--tls-verify=false"]) ["--tls-verify=false"]
                assert equal (insecure_pull_flags "docker" ["--tls-verify=false"]) []
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
