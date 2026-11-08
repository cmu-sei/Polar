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

# infra/tests/test-cluster.nu
#
# The infra tests act only on the cluster POLAR_TEST_CLUSTER_KUBECONFIG names (infra/test-cluster.nu), never the
# ambient KUBECONFIG. The helper's own rules, and the tests that would reach a cluster (tests/resolve.nu,
# tests/render-determinism.nu, and the release's determinism gate) run with a KUBECONFIG that names a cluster and a
# kubectl first on PATH that records every call and fails: without the opt-in, nothing calls it.
#
#   nu infra/tests/test-cluster.nu

use std/assert
use ../test-cluster.nu *

const INFRA = (path self | path dirname | path dirname)

# A directory holding a kubectl that appends its arguments to `calls` and fails, and a kubeconfig naming a cluster, as
# the ambient one would. Returns { bin, calls, kubeconfig }.
def trap [dir: string] {
    let bin = ($dir | path join "bin")
    mkdir $bin
    let calls = ($dir | path join "kubectl-calls")
    $"#!/bin/sh\necho \"$*\" >> ($calls)\necho 'kubectl was called' >&2\nexit 97\n" | save ($bin | path join "kubectl")
    ^chmod +x ($bin | path join "kubectl")
    let kubeconfig = ($dir | path join "ambient-kubeconfig")
    "apiVersion: v1\nkind: Config\nclusters: [{name: ambient, cluster: {server: \"https://127.0.0.1:6443\"}}]\n" | save $kubeconfig
    { bin: $bin, calls: $calls, kubeconfig: $kubeconfig }
}

def calls_of [t: record] { if ($t.calls | path exists) { open --raw $t.calls | lines } else { [] } }

# Runs nu with `args` under the trap: the ambient KUBECONFIG set, the kubectl first on PATH, and `vars` besides; the
# opt-in and any scaffold directory given by the caller's environment are removed first.
def run_trapped [t: record, args: list<string>, vars: record = {}] {
    with-env ({ KUBECONFIG: $t.kubeconfig, PATH: ($env.PATH | prepend $t.bin) } | merge $vars) {
        if not ("POLAR_TEST_CLUSTER_KUBECONFIG" in $vars) { hide-env --ignore-errors POLAR_TEST_CLUSTER_KUBECONFIG }
        hide-env --ignore-errors POLAR_SCAFFOLD_DIR
        do { ^$nu.current-exe ...$args } | complete
    }
}

def main [] {
    let tests = [
        {
            name: "no cluster is named when the variable is unset, empty or blank"
            body: {
                with-env { KUBECONFIG: "/somewhere/ambient" } {
                    hide-env --ignore-errors POLAR_TEST_CLUSTER_KUBECONFIG
                    assert equal (named) null
                    for blank in ["" "   "] {
                        with-env { POLAR_TEST_CLUSTER_KUBECONFIG: $blank } { assert equal (named) null }
                    }
                }
            }
        }
        {
            name: "a named kubeconfig is used, expanded, and one that does not exist is an error naming it"
            body: {
                let dir = (mktemp --directory)
                let file = ($dir | path join "kubeconfig")
                "" | save $file
                with-env { POLAR_TEST_CLUSTER_KUBECONFIG: $file } { assert equal (named) $file }
                let missing = ($dir | path join "gone")
                let r = (try { with-env { POLAR_TEST_CLUSTER_KUBECONFIG: $missing } { named }; null } catch {|e| $e.msg })
                assert ($r != null) "a missing kubeconfig was accepted"
                assert ($r | str contains $missing) $r
                assert ($r | str contains "POLAR_TEST_CLUSTER_KUBECONFIG") $r
                rm -rf $dir
            }
        }
        {
            name: "pin replaces the ambient KUBECONFIG: with the named one, or with a path that names no cluster"
            body: {
                let dir = (mktemp --directory)
                let file = ($dir | path join "kubeconfig")
                "" | save $file
                with-env { KUBECONFIG: "/somewhere/ambient" } {
                    hide-env --ignore-errors POLAR_TEST_CLUSTER_KUBECONFIG
                    assert equal (pin) null
                    assert equal $env.KUBECONFIG $NO_CLUSTER
                }
                with-env { KUBECONFIG: "/somewhere/ambient", POLAR_TEST_CLUSTER_KUBECONFIG: $file } {
                    assert equal (pin) $file
                    assert equal $env.KUBECONFIG $file
                }
                rm -rf $dir
            }
        }
        {
            name: "kubectl cannot load the path that names no cluster, so it falls back to nothing"
            body: {
                if (which kubectl | is-empty) { print "    (no kubectl on PATH: nothing to ask)"; return }
                let r = (with-env { KUBECONFIG: $NO_CLUSTER } { do { ^kubectl config view } | complete })
                assert ($r.exit_code != 0) "kubectl loaded it"
                assert ($r.stderr | str contains "not a directory") $r.stderr
            }
        }
        {
            name: "tests/resolve.nu with no cluster named and no scaffolds skips every test and calls no kubectl"
            body: {
                let dir = (mktemp --directory)
                let t = (trap $dir)
                let r = (run_trapped $t [($INFRA | path join "tests" "resolve.nu")])
                assert equal $r.exit_code 0 $"($r.stdout)($r.stderr)"
                assert ($r.stdout | str contains "skip") $r.stdout
                assert ($r.stdout | str contains "POLAR_TEST_CLUSTER_KUBECONFIG") $r.stdout
                assert equal (calls_of $t) []
                rm -rf $dir
            }
        }
        {
            name: "tests/render-determinism.nu with no cluster named skips and calls no kubectl"
            body: {
                let dir = (mktemp --directory)
                let t = (trap $dir)
                let r = (run_trapped $t [($INFRA | path join "tests" "render-determinism.nu")])
                assert equal $r.exit_code 0 $"($r.stdout)($r.stderr)"
                assert ($r.stdout | str contains "skip") $r.stdout
                assert equal (calls_of $t) []
                rm -rf $dir
            }
        }
        {
            name: "a named kubeconfig that does not exist stops the run before any kubectl"
            body: {
                let dir = (mktemp --directory)
                let t = (trap $dir)
                let gone = ($dir | path join "gone")
                for test in ["resolve.nu" "render-determinism.nu"] {
                    let r = (run_trapped $t [($INFRA | path join "tests" $test)] { POLAR_TEST_CLUSTER_KUBECONFIG: $gone })
                    assert ($r.exit_code != 0) $"($test) ran"
                    # The error box may wrap the path, so it is looked for by the words around it.
                    assert ($r.stderr | str contains "POLAR_TEST_CLUSTER_KUBECONFIG names") $r.stderr
                    assert ($r.stderr | str contains "which does not exist") $r.stderr
                }
                assert equal (calls_of $t) []
                rm -rf $dir
            }
        }
        {
            name: "the release's determinism gate fails without a named cluster, since a skip proves nothing"
            body: {
                let dir = (mktemp --directory)
                let t = (trap $dir)
                let r = (run_trapped $t [($INFRA | path join "release.nu") "gates" "--only" "determinism"])
                assert ($r.exit_code != 0) "the gate passed"
                assert ($"($r.stdout)($r.stderr)" | str contains "POLAR_TEST_CLUSTER_KUBECONFIG") $"($r.stdout)($r.stderr)"
                assert equal (calls_of $t) []
                rm -rf $dir
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
    if $failed > 0 {
        print -e $"($failed) of ($results | length) failed"
        exit 1
    }
    print $"all ($results | length) passed"
}
