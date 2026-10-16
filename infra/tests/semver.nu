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

# The version a set of commits points to: the rules on small commits, and the real history replayed.

use std/assert
use ../semver.nu [bump, cancel_reverts, classify, commits_between, has_breaking_footer, highest, infer, parse_conventional, parse_version, suggest_version]

const SHA_A = "aaaaaaa1111111111111111111111111111111aa"
const SHA_B = "bbbbbbb2222222222222222222222222222222bb"

# The commit the history replay ends at, so the range, and the answer, do not move as the repository grows.
const REPLAY_END = "60dc4c32be3a08c652d64968bea1fa943043a4bc"

def commit [sha: string, subject: string, body: string = "", files: list<string> = ["src/agents/lib/src/lib.rs"]] {
    { sha: $sha, author: "someone", subject: $subject, body: $body, files: $files }
}

def main [] {
    let tests = [
        {
            name: "a version is X.Y.Z with an optional v, and anything else is refused"
            body: {
                assert equal (parse_version "0.1.0") { major: 0, minor: 1, patch: 0 }
                assert equal (parse_version "v12.3.45") { major: 12, minor: 3, patch: 45 }
                for bad in ["1.2" "1.2.3-rc.1" "one.two.three" ""] {
                    let r = (try { parse_version $bad; "accepted" } catch { "refused" })
                    assert equal $r "refused" $"'($bad)' should be refused"
                }
            }
        }
        {
            name: "while the major version is 0, breaking bumps the minor and a feature or a fix bumps the patch"
            body: {
                assert equal (bump "0.1.0" "breaking") "0.2.0"
                assert equal (bump "0.1.4" "breaking") "0.2.0"
                assert equal (bump "0.1.0" "feature") "0.1.1"
                assert equal (bump "0.1.0" "fix") "0.1.1"
            }
        }
        {
            name: "from 1.0.0, breaking bumps the major, a feature the minor, a fix the patch, and each resets what is below it"
            body: {
                assert equal (bump "1.4.7" "breaking") "2.0.0"
                assert equal (bump "1.4.7" "feature") "1.5.0"
                assert equal (bump "1.4.7" "fix") "1.4.8"
            }
        }
        {
            name: "nothing significant is nothing to release"
            body: {
                assert equal (bump "1.0.0" "none") null
                assert equal (highest []) "none"
            }
        }
        {
            name: "the highest significance wins"
            body: {
                assert equal (highest ["none" "fix" "feature" "fix"]) "feature"
                assert equal (highest ["feature" "breaking" "fix"]) "breaking"
            }
        }
        {
            name: "a conventional header is read with or without a scope and a bang"
            body: {
                assert equal (parse_conventional "feat(infra): add a thing") { type: "feat", scope: "infra", bang: false }
                assert equal (parse_conventional "docs: explain it") { type: "docs", scope: "", bang: false }
                assert equal (parse_conventional "fix(a-b)!: change the wire format") { type: "fix", scope: "a-b", bang: true }
                assert equal (parse_conventional "refactor!: drop it") { type: "refactor", scope: "", bang: true }
            }
        }
        {
            name: "what is not a conventional header is not read as one"
            body: {
                for s in ["Add a thing" "Feat: capital letter" "feat add a thing" "feature: a long word" "feat(x) missing colon" ""] {
                    assert equal (parse_conventional $s) null $"'($s)' is not conventional"
                }
            }
        }
        {
            name: "a BREAKING CHANGE footer is recognised on its own line, in either spelling, and not mid-sentence"
            body: {
                assert (has_breaking_footer "text\n\nBREAKING CHANGE: it moved")
                assert (has_breaking_footer "BREAKING-CHANGE: it moved")
                assert (not (has_breaking_footer "this is not a BREAKING CHANGE: really"))
                assert (not (has_breaking_footer ""))
            }
        }
        {
            name: "a message that is not conventional is inferred from its leading verb, never as breaking"
            body: {
                assert equal (infer "Add a retry to the client" ["src/agents/lib/src/lib.rs"]) { type: "feat", how: "inferred" }
                assert equal (infer "Fix the shutdown race" ["src/agents/lib/src/lib.rs"]) { type: "fix", how: "inferred" }
                assert equal (infer "Revert the earlier change" ["src/agents/lib/src/lib.rs"]) { type: "revert", how: "inferred" }
                assert equal (infer "Bump the crate versions" ["src/agents/Cargo.lock"]) { type: "chore", how: "inferred" }
                assert equal (infer "Remove the old helper" ["src/agents/lib/src/lib.rs"]) { type: "refactor", how: "inferred" }
            }
        }
        {
            name: "a verb that says add, on docs and tests only, ships nothing"
            body: {
                assert equal (infer "Add a section to the guide" ["docs/guide.md"]) { type: "docs", how: "inferred" }
            }
        }
        {
            name: "a message that says nothing is a patch if it touches code, and nothing if it touches only docs"
            body: {
                assert equal (infer "Made some changes" ["src/agents/lib/src/lib.rs"]) { type: "fix", how: "unclassified" }
                assert equal (infer "Made some changes" ["README.md" "docs/a.md" "src/agents/x/tests/t.rs"]) { type: "docs", how: "unclassified" }
                assert equal (infer "Made some changes" []) { type: "docs", how: "unclassified" }
            }
        }
        {
            name: "a conventional commit is trusted, a non-conventional one is inferred, and a footer makes either breaking"
            body: {
                let a = (classify (commit $SHA_A "feat(x): add it") [])
                assert equal [$a.kind $a.type $a.significance] ["conventional" "feat" "feature"]
                let b = (classify (commit $SHA_B "Fixed the thing") [])
                assert equal [$b.kind $b.type $b.significance] ["inferred" "fix" "fix"]
                let c = (classify (commit $SHA_A "Reworked the protocol" "BREAKING CHANGE: old clients fail") [])
                assert equal [$c.kind $c.significance] ["unclassified" "breaking"]
                let d = (classify (commit $SHA_B "docs: x" "BREAKING CHANGE: even a docs commit says so") [])
                assert equal $d.significance "breaking"
            }
        }
        {
            name: "an override by the start of the sha can say what a message cannot, and a short sha is not trusted"
            body: {
                let c = (commit $SHA_A "Reworked the protocol")
                let o = { sha: "aaaaaaa", reason: "it broke every client", breaking: true }
                let r = (classify $c [$o])
                assert equal [$r.kind $r.significance $r.note] ["override" "breaking" "it broke every client"]
                let short = (classify $c [{ sha: "aaa", reason: "too short", breaking: true }])
                assert equal $short.significance "fix" "an override of fewer than 7 characters is ignored"
                let other = (classify (commit $SHA_B "Reworked the protocol") [$o])
                assert equal $other.significance "fix" "an override applies only to its own commit"
            }
        }
        {
            name: "an override can replace the type or leave a commit out"
            body: {
                let c = (commit $SHA_A "Made some changes")
                let t = (classify $c [{ sha: "aaaaaaa", reason: "it is a feature", type: "feat" }])
                assert equal $t.significance "feature"
                let i = (classify $c [{ sha: "aaaaaaa", reason: "ships nothing", ignore: true }])
                assert equal $i.significance "none"
                assert ($i.note | str contains "ignored")
            }
        }
        {
            name: "a revert and the commit it reverts, both in the range, stop counting; a revert of something outside it does not"
            body: {
                let a = (commit $SHA_A "feat(x): add it")
                let r = (commit $SHA_B "Revert \"feat(x): add it\"" $"This reverts commit ($SHA_A).")
                let both = (cancel_reverts [(classify $a []) (classify $r [])] [$a $r])
                assert equal ($both | get significance) ["none" "none"]
                assert equal ($both | get kind) ["cancelled" "cancelled"]
                let alone = (cancel_reverts [(classify $r [])] [$r])
                assert equal ($alone | first | get significance) "fix"
            }
        }
        {
            name: "docs, tests and housekeeping alone give nothing to release"
            body: {
                let s = (suggest_version "0.1.0" [(commit $SHA_A "docs: explain" "" ["docs/a.md"]) (commit $SHA_B "chore: tidy")] [])
                assert equal $s.next null
                assert equal $s.significance "none"
            }
        }
        {
            name: "the suggestion follows the highest significance, and lists what to look at"
            body: {
                let commits = [
                    (commit "1111111aaaa" "fix(x): mend it")
                    (commit "2222222bbbb" "Add a retry to the client")
                    (commit "3333333cccc" "Made some changes")
                    (commit "4444444dddd" "feat(y)!: break the wire" "")
                ]
                let s = (suggest_version "1.2.3" $commits [])
                assert equal $s.next "2.0.0"
                assert equal ($s.breaking | length) 1
                assert equal ($s.inferred | length) 1 "the 'Add' commit"
                assert equal ($s.unclassified | length) 1 "the 'Made some changes' commit"
                assert equal $s.commits 4
                let feature_only = (suggest_version "1.2.3" ($commits | first 2) [])
                assert equal $feature_only.next "1.3.0"
            }
        }
        {
            name: "the real history from the baseline gives 0.2.0: two breaking commits, 976 non-merge commits, many of them not conventional"
            body: {
                let base = (open ($env.FILE_PWD | path dirname | path join "release-baseline.nuon"))
                let have = (do { ^git cat-file -e $"($REPLAY_END)^{commit}" } | complete | get exit_code) == 0
                if not $have {
                    print "        (skipped: this clone does not have the history the replay needs)"
                } else {
                    let commits = (commits_between $base.commit $REPLAY_END)
                    assert equal ($commits | length) 976
                    let s = (suggest_version $base.version $commits [])
                    assert equal $s.next "0.2.0"
                    assert equal ($s.breaking | length) 2
                    assert (($s.inferred | length) > 0) "the history has commits that were not written as conventional commits"
                    assert (($s.unclassified | length) > 0) "and some that say nothing"
                    # What the heuristics add must not change the answer: with only the conventional commits it is the same.
                    let conventional = ($commits | where {|c| (parse_conventional $c.subject) != null })
                    assert equal (suggest_version $base.version $conventional [] | get next) "0.2.0"
                }
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
