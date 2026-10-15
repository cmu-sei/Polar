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

# infra/harness.nu
#
# Reads what the secret harness pods (src/agents/secret-harness) said about the credentials agent's
# secret policy, and says whether it held. The pods log one line per check:
#
#   HARNESS RESULT role=owner round=0 pass check="the owner stores a secret" detail="Ok(())"
#
# The decisions are exported and tested (infra/tests/harness.nu); `main` is the part that asks the cluster.

# The result lines in a log, as records. The log is coloured, and a line from anything else is ignored.
export def parse_results [logs: string] {
    $logs
    | ansi strip
    | lines
    | where {|l| $l | str contains "HARNESS RESULT" }
    | each {|l|
        let m = ($l | parse --regex 'HARNESS RESULT role=(?<role>\S+) round=(?<round>\d+) (?<outcome>pass|fail) check="(?<check>[^"]*)" detail="(?<detail>.*)"$')
        if ($m | is-empty) { null } else {
            let r = ($m | first)
            { role: $r.role, round: ($r.round | into int), passed: ($r.outcome == "pass"), check: $r.check, detail: $r.detail }
        }
    }
    | compact
}

# The checks of the latest round in the results.
export def latest [results: list] {
    if ($results | is-empty) { return [] }
    let last = ($results | get round | math max)
    $results | where round == $last
}

# What the two logs add up to. The owner must have gone round at least twice (the second round is the
# one that shows the intruder changed nothing), the intruder must have made its three checks, and the
# latest round of each must have passed. Returns { ok, lines } with the lines to print.
export def verdict [owner_logs: string, intruder_logs: string] {
    let owner = (parse_results $owner_logs | where role == "owner")
    let intruder = (parse_results $intruder_logs | where role == "intruder")
    let owner_last = (latest $owner)
    let intruder_last = (latest $intruder)

    mut problems = []
    if ($owner | is-empty) { $problems = ($problems | append "the owner has reported nothing") }
    if ($intruder | is-empty) { $problems = ($problems | append "the intruder has reported nothing") }
    if ($owner | is-not-empty) and (($owner | get round | math max) < 1) {
        $problems = ($problems | append "the owner has not yet gone round a second time, which is what shows the intruder changed nothing")
    }
    for c in ($owner_last | append $intruder_last | where passed == false) {
        $problems = ($problems | append $"($c.role) round ($c.round) failed: ($c.check) -- ($c.detail)")
    }

    let lines = (
        [$owner_last $intruder_last]
        | each {|round| $round | each {|c| $"  (if $c.passed { 'pass' } else { 'FAIL' })  ($c.role) round ($c.round): ($c.check)" } }
        | flatten
    )
    { ok: ($problems | is-empty), lines: $lines, problems: $problems }
}

def logs_of [workload: string] {
    let pod = (^kubectl -n polar get pods -l $"job-name=($workload)" --field-selector=status.phase=Running -o name | lines | first)
    ^kubectl -n polar logs $pod --tail=5000 | complete | get stdout
}

def main [] {
    let v = (verdict (logs_of "secret-harness-owner") (logs_of "secret-harness-intruder"))
    $v.lines | each {|l| print $l } | ignore
    if $v.ok {
        print "the credentials agent's secret policy held: the owner kept its secret and the intruder got nothing"
    } else {
        for p in $v.problems { print -e $"  not yet: ($p)" }
        exit 1
    }
}
