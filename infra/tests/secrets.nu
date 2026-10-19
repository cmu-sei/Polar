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

# infra/tests/secrets.nu
#
# Tests of the decision in infra/secrets-lib.nu that matters when SOPS will not decrypt: whether
# the secrets step can go on. It can when every secret the cluster needs is already there; it
# must stop, and say why, when one is missing and could not have been made.
#
#   nu infra/tests/secrets.nu

use std/assert
use ../secrets-lib.nu *

const REQUIRED = [
    { namespace: "polar-graph", name: "neo4j-secret", key: "neo4j_auth" }
    { namespace: "polar", name: "polar-graph-pw", key: "graph_password" }
]

const BOTH_THERE = ["polar-graph/neo4j-secret" "polar/polar-graph-pw"]

def failure [file: string, reason: string] { { file: $file, reason: $reason } }

# A target directory whose shared SOPS file is not a SOPS file, so it cannot be decrypted by anyone.
def broken_target [] {
    let dir = (mktemp --directory)
    "creation_rules: []\n" | save --force ($dir | path join ".sops.yaml")
    "neo4j_auth: not-encrypted\n" | save --force ($dir | path join "secrets.sops.yaml")
    $dir
}

def main [] {
    let tests = [
        {
            name: "a file is decrypted with no .sops.yaml beside it, because it knows its own recipients"
            body: {
                let dir = (mktemp -d)
                let key = ($dir | path join "k")
                ^ssh-keygen -q -t ed25519 -N "" -f $key
                let pub = (open --raw $"($key).pub" | split row " " | first 2 | str join " ")
                $"creation_rules:\n  - path_regex: secrets\\.sops\\.yaml$\n    age: ($pub)\n" | save ($dir | path join ".sops.yaml")
                "neo4j_auth: neo4j/x\n" | save ($dir | path join "secrets.sops.yaml")
                with-env { SOPS_CONFIG: ($dir | path join ".sops.yaml") } { ^sops --encrypt --in-place ($dir | path join "secrets.sops.yaml") }
                rm ($dir | path join ".sops.yaml")
                let got = (with-env { SOPS_AGE_SSH_PRIVATE_KEY_FILE: $key } { decrypt_sops $dir "secrets.sops.yaml" })
                rm -rf $dir
                assert equal $got.failure null
                assert equal $got.data.neo4j_auth "neo4j/x"
            }
        }
        {
            name: "a generated token is long, made of letters and digits, and different each time"
            body: {
                let a = (random_token)
                let b = (random_token)
                assert equal ($a | str length) 48
                assert ($a =~ '^[A-Za-z0-9]+$')
                assert ($a != $b)
                assert equal (random_token 16 | str length) 16
            }
        }
        {
            name: "a target whose SOPS file will not decrypt is told so by the check, with the way out, before anything is built"
            body: {
                let dir = (broken_target)
                let found = (secrets_check $dir [])
                assert equal ($found.problems | length) 2
                for p in $found.problems {
                    assert ($p | str contains "would not decrypt") $p
                    assert ($p | str contains "SOPS_AGE_SSH_PRIVATE_KEY_FILE") $p
                    assert ($p | str contains "SOPS_AGE_BINARY") $p
                }
                assert ($found.failures | any {|f| $f.file == "secrets.sops.yaml" })
                rm -rf $dir
            }
        }
        {
            name: "the check lets a resume through when the secrets already exist, even though the file will not decrypt"
            body: {
                let dir = (broken_target)
                let found = (secrets_check $dir $BOTH_THERE)
                assert equal $found.problems []
                assert ($found.failures | is-not-empty)
                rm -rf $dir
            }
        }
        {
            name: "the check names what is missing when there is nothing to decrypt and nothing exists"
            body: {
                let dir = (mktemp --directory)
                let found = (secrets_check $dir [])
                assert equal ($found.problems | length) 2
                assert ($found.problems | all {|p| $p | str contains "is not in the decrypted files" })
                rm -rf $dir
            }
        }
        {
            name: "the secrets every target needs are written once"
            body: {
                assert equal ($REQUIRED_SECRETS | get name | sort) ["neo4j-secret" "polar-graph-pw"]
            }
        }
        {
            name: "a decrypt failure is harmless when every required secret already exists"
            body: {
                let problems = (required_secret_problems $REQUIRED $BOTH_THERE {} [(failure "secrets.sops.yaml" "no identity matched")])
                assert equal $problems []
            }
        }
        {
            name: "a missing secret that could not be decrypted is a problem that says what to check"
            body: {
                let problems = (required_secret_problems $REQUIRED ["polar/polar-graph-pw"] {} [(failure "secrets.sops.yaml" "no identity matched")])
                assert equal ($problems | length) 1
                let p = ($problems | first)
                assert ($p | str contains "polar-graph/neo4j-secret")
                assert ($p | str contains "secrets.sops.yaml")
                assert ($p | str contains "no identity matched")
                assert ($p | str contains "SOPS_AGE_SSH_PRIVATE_KEY_FILE")
            }
        }
        {
            name: "a missing secret whose value was decrypted is no problem, it will be created"
            body: {
                let problems = (required_secret_problems $REQUIRED [] { neo4j_auth: "neo4j/x", graph_password: "x" } [])
                assert equal $problems []
            }
        }
        {
            name: "a missing secret with no value in files that decrypted fine names the key, not the keys to SOPS"
            body: {
                let problems = (required_secret_problems $REQUIRED ["polar-graph/neo4j-secret"] { other: "y" } [])
                assert equal ($problems | length) 1
                let p = ($problems | first)
                assert ($p | str contains "graph_password")
                assert (not ($p | str contains "SOPS_AGE"))
            }
        }
        {
            name: "every missing secret is reported, not only the first"
            body: {
                let problems = (required_secret_problems $REQUIRED [] {} [(failure "user.sops.yaml" "boom")])
                assert equal ($problems | length) 2
            }
        }
        {
            name: "an empty value counts as no value"
            body: {
                let problems = (required_secret_problems $REQUIRED ["polar/polar-graph-pw"] { neo4j_auth: "" } [])
                assert equal ($problems | length) 1
            }
        }
        {
            name: "the first line of a long error is what is shown"
            body: {
                let line = (first_line "Group 0: FAILED\n  ssh-rsa AAAA...\n  more")
                assert equal $line "Group 0: FAILED"
            }
        }
    ]

    let results = ($tests | each {|t|
        try {
            do $t.body
            { name: $t.name, error: null }
        } catch {|e|
            { name: $t.name, error: $e.msg }
        }
    })
    for r in $results {
        if $r.error == null {
            print $"  ok    ($r.name)"
        } else {
            print -e $"  FAIL  ($r.name): ($r.error)"
        }
    }
    let failed = ($results | where error != null | length)
    if $failed > 0 {
        print -e $"($failed) of ($results | length) failed"
        exit 1
    }
    print $"all ($results | length) passed"
}
