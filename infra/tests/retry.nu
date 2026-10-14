#!/usr/bin/env nu
# infra/tests/retry.nu
#
# The helper up.nu uses for what goes through a port-forward or waits on a pod that is being replaced:
# try again for a while, and stop at once on a failure that trying again cannot fix.
#
#   nu infra/tests/retry.nu

use std/assert
use ../retry.nu *

def main [] {
    let tests = [
        {
            name: "a first success comes back at once and is not retried"
            body: {
                let r = (with_retries 5 0sec {|n| { ok: true, final: false, detail: "fine" } })
                assert $r.ok
                assert equal $r.tries 1
                assert equal $r.detail "fine"
            }
        }
        {
            name: "a failure that passes on the third try is a success after three"
            body: {
                let r = (with_retries 5 0sec {|n| if $n < 3 { { ok: false, final: false, detail: $"not yet ($n)" } } else { { ok: true, final: false, detail: "there" } } })
                assert $r.ok
                assert equal $r.tries 3
            }
        }
        {
            name: "a failure that never passes uses every attempt and says what the last one said"
            body: {
                let r = (with_retries 4 0sec {|n| { ok: false, final: false, detail: $"refused ($n)" } })
                assert (not $r.ok)
                assert equal $r.tries 4
                assert equal $r.detail "refused 4"
            }
        }
        {
            name: "a final failure stops at once, since trying again cannot fix it"
            body: {
                let r = (with_retries 5 0sec {|n| { ok: false, final: true, detail: "403" } })
                assert (not $r.ok)
                assert equal $r.tries 1
                assert equal $r.detail "403"
            }
        }
        {
            name: "the pause is taken between attempts and not after the last"
            body: {
                let started = (date now)
                let r = (with_retries 3 200ms {|n| { ok: false, final: false, detail: "x" } })
                let took = ((date now) - $started)
                assert ($took >= 400ms) $"took ($took)"
                assert ($took < 590ms) $"took ($took)"
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
