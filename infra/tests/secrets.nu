#!/usr/bin/env nu
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

def main [] {
    let tests = [
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
