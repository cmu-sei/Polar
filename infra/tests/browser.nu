#!/usr/bin/env nu
# infra/tests/browser.nu
#
# The decisions behind step 10 of up.nu (the browser certificate and the Neo4j password), kept apart from the
# browser profile and the cluster so they can be tested. The step looks first, without changing anything: it
# stops for a person only for what is missing, and says which.
#
#   nu infra/tests/browser.nu

use std/assert
use ../browser.nu *

const CA_A = "AA:BB:CC:00:11"
const CA_B = "DD:EE:FF:22:33"

def has [problems: list, needle: string] { $problems | any {|p| $p | str contains $needle } }

def main [] {
    let tests = [
        {
            name: "a browser that trusts this cluster's CA and holds the user's current certificate has nothing to do"
            body: {
                let p = (browser_problems { ca: $CA_A, serial: "0F" } { ca: $CA_A, serial: "0F" } "alice")
                assert equal $p []
            }
        }
        {
            name: "a browser with no Polar CA is told so, with the way out"
            body: {
                let p = (browser_problems { ca: null, serial: null } { ca: $CA_A, serial: "0F" } "alice")
                assert (has $p "no Polar Internal CA")
                assert (has $p "just user-cert-install alice")
            }
        }
        {
            name: "a CA of the same name from another cluster is named as such, since the browser reports it as a bad signature"
            body: {
                let p = (browser_problems { ca: $CA_B, serial: "0F" } { ca: $CA_A, serial: "0F" } "alice")
                assert (has $p "another cluster")
                assert (has $p "SEC_ERROR_BAD_SIGNATURE")
                assert (has $p $CA_B)
                assert (has $p $CA_A)
            }
        }
        {
            name: "a certificate for the user that is not the current one is an older one"
            body: {
                let p = (browser_problems { ca: $CA_A, serial: "11" } { ca: $CA_A, serial: "0F" } "alice")
                assert (has $p "older")
                assert (not (has $p "no Polar Internal CA"))
            }
        }
        {
            name: "no certificate for the user is its own problem, and a good CA does not hide it"
            body: {
                let p = (browser_problems { ca: $CA_A, serial: null } { ca: $CA_A, serial: "0F" } "alice")
                assert equal ($p | length) 1
                assert (has $p "no certificate for alice")
            }
        }
        {
            name: "fingerprints are compared without regard to case or colons"
            body: {
                assert (same_fingerprint "AA:BB:cc" "aabbCC")
                assert (not (same_fingerprint "AA:BB:cc" "aabbCD"))
                assert (not (same_fingerprint null "aabbcc"))
            }
        }
        {
            name: "Neo4j answering 200 to the Secret's password means the password is already set"
            body: { assert equal (neo4j_password_state 200 "") "set" }
        }
        {
            name: "Neo4j asking for a new password means it is still the default one"
            body: {
                assert equal (neo4j_password_state 403 '{"errors":[{"code":"Neo.ClientError.Security.CredentialsExpired","message":"The credentials you provided were valid, but must be changed before you can use this instance."}]}') "default"
            }
        }
        {
            name: "a refused password is wrong and not default, so the step does not tell anyone to set it again"
            body: {
                assert equal (neo4j_password_state 401 '{"errors":[{"code":"Neo.ClientError.Security.Unauthorized"}]}') "different"
            }
        }
        {
            name: "no answer from Neo4j is unknown, and never taken for set or default"
            body: {
                assert equal (neo4j_password_state 0 "") "unreachable"
                assert equal (neo4j_password_state 502 "") "unknown"
                # a refusal that is not a request to change the password is not the default password
                assert equal (neo4j_password_state 403 '{"errors":[{"code":"Neo.ClientError.Security.Forbidden"}]}') "unknown"
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
