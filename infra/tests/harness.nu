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

# infra/tests/harness.nu
#
#   nu infra/tests/harness.nu

use std/assert
use ../harness.nu *

def line [role: string, round: int, outcome: string, check: string, detail: string = "ok"] {
    $'2026-10-12T20:00:00Z  INFO HARNESS RESULT role=($role) round=($round) ($outcome) check="($check)" detail="($detail)"'
}

def owner_logs [rounds: int, outcome: string = "pass"] {
    0..<$rounds | each {|r|
        if $r == 0 {
            [(line owner 0 pass "the owner stores a secret") (line owner 0 pass "the owner retrieves its own secret") (line owner 0 pass "the owner replaces its own secret") (line owner 0 pass "the owner retrieves the replacement")]
        } else {
            [(line owner $r $outcome "the owner still has its secret, unchanged")]
        }
    } | flatten | str join "\n"
}

def intruder_logs [outcome: string = "pass"] {
    [(line intruder 0 $outcome "the intruder is denied another identity's secret") (line intruder 0 pass "a name nothing holds is refused the same way") (line intruder 0 pass "the intruder cannot take the name over")] | str join "\n"
}

def main [] {
    let tests = [
        {
            name: "result lines are read out of a log with other lines in it, coloured or not"
            body: {
                let esc = (char --integer 27)
                let logs = $"noise\n($esc)[2m2026($esc)[0m (line owner 0 pass 'c1')\nmore noise\n(line intruder 3 fail 'c2' 'said no')"
                let r = (parse_results $logs)
                assert equal ($r | length) 2
                assert equal ($r | get role) [owner intruder]
                assert equal ($r | get round) [0 3]
                assert equal ($r | get passed) [true false]
                assert equal ($r | last | get detail) "said no"
            }
        }
        {
            name: "a log with no results has none"
            body: { assert equal (parse_results "nothing here\n") [] }
        }
        {
            name: "the latest round is the one with the highest number"
            body: {
                let r = (parse_results (owner_logs 3))
                assert equal (latest $r | length) 1
                assert equal (latest $r | first | get round) 2
                assert equal (latest []) []
            }
        }
        {
            name: "when the owner has gone round twice and the intruder has been refused, the policy held"
            body: {
                let v = (verdict (owner_logs 2) (intruder_logs))
                assert $v.ok
                assert equal $v.problems []
                assert (($v.lines | length) > 0)
            }
        }
        {
            name: "nothing reported is not a pass"
            body: {
                let v = (verdict "" "")
                assert (not $v.ok)
                assert equal ($v.problems | length) 2
            }
        }
        {
            name: "an owner that has gone round only once cannot yet show the intruder changed nothing"
            body: {
                let v = (verdict (owner_logs 1) (intruder_logs))
                assert (not $v.ok)
                assert ($v.problems | any {|p| $p | str contains "second time" })
            }
        }
        {
            name: "an intruder check that failed fails the whole, and says which"
            body: {
                let v = (verdict (owner_logs 2) (intruder_logs "fail"))
                assert (not $v.ok)
                assert ($v.problems | any {|p| $p | str contains "the intruder is denied another identity's secret" })
            }
        }
        {
            name: "the owner losing its secret fails the whole"
            body: {
                let v = (verdict (owner_logs 3 "fail") (intruder_logs))
                assert (not $v.ok)
                assert ($v.problems | any {|p| $p | str contains "owner round 2 failed" })
            }
        }
        {
            name: "an early failure that a later round has recovered from does not fail it"
            body: {
                let owner = $"(line owner 0 fail 'the owner stores a secret')\n(owner_logs 3)"
                let v = (verdict $owner (intruder_logs))
                assert $v.ok
            }
        }
    ]

    let results = ($tests | each {|t|
        let r = (try { do $t.body; null } catch {|e| $e.msg })
        { name: $t.name, error: $r }
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
