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
            name: "a usable answer from Neo4j is `ok`, and a refusal is `refused`: a wrong password is the 401"
            body: {
                assert equal (neo4j_answer 200 '{"results":[{"columns":["1"],"data":[{"row":[1],"meta":[null]}]}],"errors":[]}') "ok"
                assert equal (neo4j_answer 401 '{"errors":[{"code":"Neo.ClientError.Security.Unauthorized","message":"Invalid credential."}]}') "refused"
            }
        }
        {
            name: "valid credentials that must be changed are `expired`, and Neo4j reports that as a 200 with the code in the body"
            body: {
                # The body is what a Neo4j started with its default credentials really answered (observed, not
                # recalled): the status is 200, and the error is in the body.
                let body = '{"results":[],"errors":[{"code":"Neo.ClientError.Security.CredentialsExpired","message":"ACCESS on database \'neo4j\' is not allowed.\n\nThe credentials you provided were valid, but must be changed before you can use this instance."}]}'
                assert equal (neo4j_answer 200 $body) "expired"
            }
        }
        {
            name: "no answer is `unreachable`, anything else is `unknown`, and a 403 is not taken for a request to change the password"
            body: {
                assert equal (neo4j_answer 0 "") "unreachable"
                assert equal (neo4j_answer 502 "") "unknown"
                assert equal (neo4j_answer 403 '{"errors":[{"code":"Neo.ClientError.Security.CredentialsExpired"}]}') "unknown"
            }
        }
        {
            name: "the Secret's password working means the password is set"
            body: { assert equal (password_verdict "ok" null) "set" }
        }
        {
            name: "a Neo4j that refuses the Secret's password but accepts neo4j/neo4j as valid-but-expired is still on its default password"
            body: {
                # The case step 10 exists for, and the one the first version got backwards: the Secret's password is
                # refused (401) because Neo4j still has the default.
                assert equal (password_verdict "refused" "expired") "default"
            }
        }
        {
            name: "the Secret's password accepted but expired also needs a first change"
            body: { assert equal (password_verdict "expired" null) "default" }
        }
        {
            name: "a Neo4j that refuses both has a password of its own, which is not the default, so nobody is told to set it again"
            body: {
                assert equal (password_verdict "refused" "refused") "different"
                assert equal (password_verdict "refused" null) "different"
            }
        }
        {
            name: "no answer is never taken for set or default, whichever probe it was"
            body: {
                assert equal (password_verdict "unreachable" null) "unreachable"
                assert equal (password_verdict "refused" "unreachable") "unreachable"
                assert equal (password_verdict "unknown" null) "unknown"
            }
        }
        {
            name: "a Neo4j that is still starting is waited for: the probe is repeated until something answers"
            body: {
                let f = (mktemp)
                "0" | save --force $f
                let probe = {||
                    let n = ((open --raw $f | into int) + 1)
                    $"($n)" | save --force $f
                    if $n < 3 { { code: 0, body: "" } } else { { code: 200, body: "{}" } }
                }
                let r = (probe_until_answered 5 1ms $probe)
                let calls = (open --raw $f | into int)
                rm -f $f
                assert equal $r.code 200
                assert equal $calls 3
            }
        }
        {
            name: "a Neo4j that never answers is given up on after the attempts, reporting no answer"
            body: {
                let f = (mktemp)
                "0" | save --force $f
                let probe = {||
                    $"((open --raw $f | into int) + 1)" | save --force $f
                    { code: 0, body: "" }
                }
                let r = (probe_until_answered 4 1ms $probe)
                let calls = (open --raw $f | into int)
                rm -f $f
                assert equal $r.code 0
                assert equal $calls 4
            }
        }
        {
            name: "any answer ends the wait, a refusal included: trying again cannot change it"
            body: {
                let f = (mktemp)
                "0" | save --force $f
                let probe = {||
                    $"((open --raw $f | into int) + 1)" | save --force $f
                    { code: 401, body: "" }
                }
                let r = (probe_until_answered 5 1ms $probe)
                let calls = (open --raw $f | into int)
                rm -f $f
                assert equal $r.code 401
                assert equal $calls 1
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
