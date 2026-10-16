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

# The decisions a release makes: versions, what is checked first, which gates run, and what the secret scan may show.

use std/assert
use ../release.nu [compare_versions, looks_like_placeholder, preflight_problems, release_message, secret_rows, select_gates, unaccepted, version_problems]

def config [] {
    { gates: [
        { name: "unit", blocking: true, what: "u" }
        { name: "deny", blocking: true, what: "d" }
        { name: "clippy", blocking: false, what: "c" }
    ] }
}

def main [] {
    let tests = [
        {
            name: "versions compare by major, then minor, then patch, as numbers"
            body: {
                assert equal (compare_versions "0.1.0" "0.1.0") 0
                assert equal (compare_versions "0.2.0" "0.1.9") 1
                assert equal (compare_versions "0.1.9" "0.2.0") (-1)
                assert equal (compare_versions "1.0.0" "0.99.99") 1
                assert equal (compare_versions "0.1.10" "0.1.9") 1 "ten is after nine, not before"
            }
        }
        {
            name: "a version that is not after the previous release, or already tagged, or malformed, is refused with the reason"
            body: {
                assert equal (version_problems "0.2.0" "0.1.0" []) []
                assert ((version_problems "0.1.0" "0.1.0" []) | any {|p| $p | str contains "not after the previous release"})
                assert ((version_problems "0.0.9" "0.1.0" []) | any {|p| $p | str contains "not after"})
                assert ((version_problems "0.2.0" "0.1.0" ["v0.2.0"]) | any {|p| $p | str contains "already exists"})
                assert ((version_problems "next" "0.1.0" []) | any {|p| $p | str contains "not a version"})
            }
        }
        {
            name: "a dirty tree or a detached HEAD stops a release, and --allow-dirty lets the dirty tree through"
            body: {
                assert equal (preflight_problems { dirty: false, branch: "main" } false) []
                assert ((preflight_problems { dirty: true, branch: "main" } false) | any {|p| $p | str contains "uncommitted"})
                assert equal (preflight_problems { dirty: true, branch: "main" } true) []
                assert ((preflight_problems { dirty: false, branch: "" } true) | any {|p| $p | str contains "detached"})
            }
        }
        {
            name: "all gates run unless some are chosen, a skipped gate is named, and an unknown gate is an error"
            body: {
                assert equal (select_gates (config) [] [] | get run | get name) ["unit" "deny" "clippy"]
                let s = (select_gates (config) [] ["deny"])
                assert equal ($s.run | get name) ["unit" "clippy"]
                assert equal $s.skipped ["deny"]
                assert equal (select_gates (config) ["clippy"] [] | get run | get name) ["clippy"]
                let r = (try { select_gates (config) ["bogus"] []; "accepted" } catch {|e| $e.msg })
                assert ($r | str contains "unknown gate")
            }
        }
        {
            name: "the secret rows say where a finding is and never hold what it matched"
            body: {
                let secret_text = "ghp_9fK2mQx7LbV3nT8cYw1ZsRj4DhPa0eUo"
                let report = [{
                    finding_id: "df3fbd860df2aaaa", rule_name: "GitHub Personal Access Token", num_matches: 2
                    matches: [{
                        provenance: [{ kind: "git_repo", first_commit: { commit_metadata: { commit_id: "48f48cb9e1234567" }, blob_path: "src/x/tests/run.fish" } }]
                        snippet: { matching: $secret_text }
                    }]
                } {
                    finding_id: "9f03279cbbbb", rule_name: "Generic Secret", num_matches: 1
                    matches: [{ provenance: [{ kind: "file", path: "docs/a.md" }], snippet: { matching: "changeme-example" } }]
                }]
                let rows = (secret_rows $report)
                assert equal ($rows | length) 2
                let first = ($rows | where rule == "GitHub Personal Access Token" | first)
                assert equal [$first.commit $first.path $first.count] ["48f48cb" "src/x/tests/run.fish" 2]
                assert equal $first.placeholder_like false
                assert ((($rows | to nuon) | str contains $secret_text) == false) "the matched text is not in the rows"
                assert (($rows | where rule == "Generic Secret" | first | get placeholder_like)) "an obvious example is marked"
                assert equal ($rows | where rule == "Generic Secret" | first | get path) "docs/a.md" "a file finding has its path"
            }
        }
        {
            name: "what looks like an example is told from what does not, roughly"
            body: {
                assert (looks_like_placeholder "your-token-here")
                assert (looks_like_placeholder "ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx")
                assert (looks_like_placeholder "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                assert (not (looks_like_placeholder "ghp_9fK2mQx7LbV3nT8cYw1ZsRj4DhPa0eUo"))
                assert (not (looks_like_placeholder ""))
            }
        }
        {
            name: "a finding is accepted by the start of its id, of at least 8 characters, and others stay open"
            body: {
                let rows = [{ id: "df3fbd860df2aaaa", rule: "r", commit: "c", path: "p", count: 1, placeholder_like: true } { id: "768a1736b7ca", rule: "r", commit: "c", path: "q", count: 1, placeholder_like: false }]
                assert equal (unaccepted $rows [] | length) 2
                assert equal (unaccepted $rows [{ id: "df3fbd86", reason: "an example" }] | get id) ["768a1736b7ca"]
                assert equal (unaccepted $rows [{ id: "df3f", reason: "too short to be trusted" }] | length) 2
                assert equal (unaccepted $rows [{ id: "df3fbd860df2aaaa", reason: "x" } { id: "768a1736b7ca", reason: "y" }] | length) 0
            }
        }
        {
            name: "the release commit is a chore, so it counts for nothing toward the next version"
            body: {
                assert equal (release_message "0.2.0") "chore(release): v0.2.0"
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
