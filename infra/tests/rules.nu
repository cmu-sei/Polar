#!/usr/bin/env nu
# infra/tests/rules.nu
#
# Tests of the rules in infra/rules.nu: the checks `resolve.nu check` runs over a
# workload's health and shutdown settings. The rules are pure functions of a record,
# so each case here is a record and the problems it should produce.
#
#   nu infra/tests/rules.nu
#
# The rules mirror the checks the Rust side makes when an agent starts
# (polar-health HealthConfig::validate), so a bad combination fails at render time
# instead of as a crash loop. Where a case below is an exact boundary, it is the same
# boundary the Rust tests pin.

use std/assert
use ../rules.nu *

# The settings every agent gets today, which the Rust defaults match.
const GOOD_HEALTH = {
    tick_secs: 30
    expiry_secs: 60
    rejuvenation_secs: 300
    stale_secs: 90
    jitter_window_secs: 180
}

const GOOD_SHUTDOWN = { budget_secs: 20, grace_margin_secs: 10, termination_grace_secs: 30 }

def has [problems: list<string>, needle: string] {
    $problems | any {|p| $p | str contains $needle }
}

def main [] {
    let tests = {
        "the defaults have no health problems": {
            assert equal (health_problems "w" $GOOD_HEALTH) []
        }

        "a zero tick is a problem": {
            let p = (health_problems "w" ($GOOD_HEALTH | update tick_secs 0))
            assert (has $p "health.tick_secs must be at least 1")
        }

        "expiry must be positive": {
            let p = (health_problems "w" ($GOOD_HEALTH | update expiry_secs 0))
            assert (has $p "health.expiry_secs must be positive")
        }

        "rejuvenation must exceed expiry": {
            let p = (health_problems "w" ($GOOD_HEALTH | update rejuvenation_secs 60))
            assert (has $p "must be greater than health.expiry_secs")
        }

        "when the order is wrong the room is not reported as well": {
            let p = (health_problems "w" ($GOOD_HEALTH | update rejuvenation_secs 60))
            assert equal ($p | length) 1
        }

        "a rejuvenation needs room for two ticks and the jitter window, exactly": {
            # 2 x 30 + 180 = 240, and 300 - 60 is 240: it fits, with nothing to spare.
            assert equal (health_problems "w" $GOOD_HEALTH) []
            let p = (health_problems "w" ($GOOD_HEALTH | update rejuvenation_secs 299))
            assert (has $p "hard expiry floor")
            assert (has $p "240s")
        }

        "a longer tick needs more room": {
            assert (health_problems "w" ($GOOD_HEALTH | update tick_secs 31) | is-not-empty)
            let ok = ($GOOD_HEALTH | update tick_secs 31 | update rejuvenation_secs 302 | update stale_secs 90)
            assert equal (health_problems "w" $ok) []
        }

        "the probe must tolerate a missed tick": {
            let p = (health_problems "w" ($GOOD_HEALTH | update stale_secs 60))
            assert (has $p "health.stale_secs (60) must exceed twice health.tick_secs (30)")
            assert equal (health_problems "w" ($GOOD_HEALTH | update stale_secs 61)) []
        }

        "every problem names the workload": {
            let p = (health_problems "kube-consumer" ($GOOD_HEALTH | update tick_secs 0))
            assert ($p | all {|x| $x | str starts-with "kube-consumer: " })
        }

        "several problems are all reported": {
            let p = (health_problems "w" ($GOOD_HEALTH | update tick_secs 0 | update expiry_secs 0))
            assert (($p | length) >= 2)
        }

        "the default shutdown settings have no problems": {
            assert equal (shutdown_problems "w" $GOOD_SHUTDOWN) []
        }

        "a budget under a second is a problem": {
            let p = (shutdown_problems "w" ($GOOD_SHUTDOWN | update budget_secs 0 | update termination_grace_secs 10))
            assert (has $p "shutdown.budget_secs must be a whole number of seconds, at least 1")
        }

        "the grace period must exceed the budget": {
            let p = (shutdown_problems "w" ($GOOD_SHUTDOWN | update termination_grace_secs 20))
            assert (has $p "must exceed the shutdown budget")
            assert equal (shutdown_problems "w" ($GOOD_SHUTDOWN | update termination_grace_secs 21)) []
        }

        "the margin must be at least a second": {
            let p = (shutdown_problems "w" ($GOOD_SHUTDOWN | update grace_margin_secs 0 | update termination_grace_secs 20))
            assert (has $p "shutdown.grace_margin_secs must be at least 1")
        }
    }

    let results = ($tests | transpose name body | each {|t|
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
