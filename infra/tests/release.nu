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
use ../release.nu [compare_versions, dev_version, looks_like_placeholder, preflight_problems, read_version_text, release_diff_problems, release_message, release_plan_lines, release_steps, release_trailers, secret_rows, select_gates, unaccepted, version_file_status, version_file_text, version_problems]

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
            name: "a release's commits end with the trailers whoever cuts it gives, one a line, and with none when none are given"
            body: {
                assert equal (with-env { POLAR_RELEASE_TRAILERS: null } { release_trailers }) []
                assert equal (with-env { POLAR_RELEASE_TRAILERS: "" } { release_trailers }) []
                assert equal (with-env { POLAR_RELEASE_TRAILERS: "A: one\n\n  B: two  \n" } { release_trailers }) ["A: one" "B: two"]
            }
        }
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
        {
            name: "VERSION reads a release or a development version, and nothing else"
            body: {
                assert equal (read_version_text "1.0.0+dev\n") { version: "1.0.0+dev", release: "1.0.0", development: true }
                assert equal (read_version_text "2.3.4") { version: "2.3.4", release: "2.3.4", development: false }
                for bad in ["" "v1.0.0" "1.0" "1.0.0.0" "01.0.0" "1.0.0-rc.1" "1.0.0+dev2" "1.0.0+DEV" "1.0.0+build" "1.0.0 +dev"] {
                    let r = (try { read_version_text $bad; "accepted" } catch {|e| $e.msg })
                    assert ($r | str contains "not X.Y.Z or X.Y.Z+dev") $"($bad) was accepted"
                }
                assert equal (version_file_text "1.2.0") "1.2.0\n"
                assert equal (version_file_text "1.2.0+dev") "1.2.0+dev\n"
                assert equal (dev_version "1.2.0") "1.2.0+dev"
                let twice = (try { dev_version "1.2.0+dev"; "accepted" } catch {|e| $e.msg })
                assert ($twice | str contains "already a development version")
            }
        }
        {
            name: "a release writes the version into VERSION, commits the lock and the changelog with the tag, then writes the development version"
            body: {
                let s = (release_steps "1.2.0")
                assert equal ($s | get step) ["prepare" "release" "development"]
                assert equal ($s | get version_file) ["1.2.0" null "1.2.0+dev"] "what each commit writes into VERSION"
                assert equal ($s | get message) ["chore(release): prepare v1.2.0" "chore(release): v1.2.0" "chore: 1.2.0+dev"]
                assert equal ($s | get tag) [null "v1.2.0" null] "only the release commit is tagged"
                assert equal ($s | where step == "prepare" | first | get files) ["VERSION"]
                assert equal ($s | where step == "development" | first | get files) ["VERSION"]
                let tagged = ($s | where step == "release" | first | get files)
                assert ("VERSION" not-in $tagged) "the tagged commit leaves VERSION as the prepare commit wrote it"
                assert equal ($tagged | sort) ["CHANGELOG.md" "infra/images.lock.nuon"]
                # Every message is conventional, and none counts toward the next version.
                for m in ($s | get message) { assert ($m | str starts-with "chore") $m }
                let r = (try { release_steps "1.2.0+dev"; "accepted" } catch {|e| $e.msg })
                assert ($r | str contains "already a development version")
            }
        }
        {
            name: "the tagged commit may change only the lock and the changelog since the prepare commit"
            body: {
                assert equal (release_diff_problems ["infra/images.lock.nuon" "CHANGELOG.md"] "1.2.0") []
                assert equal (release_diff_problems [] "1.2.0") []
                let p = (release_diff_problems ["CHANGELOG.md" "VERSION" "src/agents/lib/src/lib.rs"] "1.2.0")
                assert equal ($p | length) 2
                assert ($p | any {|x| $x | str contains "VERSION" })
                assert ($p | any {|x| $x | str contains "src/agents/lib/src/lib.rs" })
            }
        }
        {
            name: "VERSION agrees with the last release when it reads that release, with or without +dev"
            body: {
                assert (version_file_status "1.0.0+dev\n" "1.0.0" | get agrees)
                assert ((version_file_status "1.0.0+dev\n" "1.0.0" | get note) | str contains "development since")
                assert (version_file_status "1.0.0\n" "1.0.0" | get agrees)
                let ahead = (version_file_status "1.1.0+dev" "1.0.0")
                assert (not $ahead.agrees)
                assert ($ahead.note | str contains "1.0.0+dev")
                assert (not (version_file_status "1.0.0+dev" "1.1.0" | get agrees))
                let junk = (version_file_status "next" "1.0.0")
                assert (not $junk.agrees)
                assert equal $junk.version "next"
            }
        }
        {
            name: "a dry run says every commit it would make, and what it would write into VERSION"
            body: {
                let text = (release_plan_lines "1.2.0" false | str join "\n")
                for expected in ["write 1.2.0 into VERSION" "chore(release): prepare v1.2.0" "chore(release): v1.2.0" "tag v1.2.0" "write 1.2.0+dev into VERSION" "chore: 1.2.0+dev" "push nothing" "with the prepare commit as what they were built from"] {
                    assert ($text | str contains $expected) $"missing: ($expected)"
                }
                let skipped = (release_plan_lines "1.2.0" true | str join "\n")
                assert ($skipped | str contains "not built from the prepare commit")
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
