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

# infra/semver.nu
#
# The next platform version, calculated from what changed since the previous release. One version for every Polar image:
# a release is decided as a whole, not per container (see infra/lock.nu for what a release is made of).
#
#   nu infra/semver.nu suggest [--from REV]     the evidence, and the version it points to
#
# The previous release is the latest `vX.Y.Z` tag reachable from HEAD, and until there is one, the baseline in
# infra/release-baseline.nuon. Every non-merge commit after it is classified, and the highest significance wins.
#
# Classification, in this order, so that people who do not write conventional commits are counted too:
#   1. an override for the commit (infra/release-overrides.nuon), which can say what a message cannot (that it breaks)
#   2. a conventional header, `type(scope)!: subject`, which is trusted
#   3. a git revert; it and the commit it reverts cancel out when both are in the range
#   4. the subject's leading verb ("Add ..." a feature, "Fix ..." a fix), marked INFERRED, never above a feature, and
#      never breaking: nothing in a message that is not conventional can say so
#   5. the files it touches: only docs and tests means nothing ships; anything else is a patch, marked UNCLASSIFIED
# A `BREAKING CHANGE:` footer makes any commit breaking, however it was written.
#
# Significance: breaking, feature, fix or none. While the major version is 0, breaking bumps the minor version and a
# feature or a fix bumps the patch (the 0.x convention); from 1.0.0, breaking bumps the major, a feature the minor, a fix the
# patch. Nothing but `none` in the range means there is nothing to release. Whether the artifacts really changed is the
# release step's other check (the lock's digests, and a diff of the render); this decides how much, not whether.

const CONVENTIONAL = r#'^(?<type>feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)(?<scope>\([^)]*\))?(?<bang>!)?:\s'#
const BREAKING_FOOTER = r#'(?m)^(?<b>BREAKING[ -]CHANGE):'#
const BREAKING_NOTE = r#'(?ms)^BREAKING[ -]CHANGE:[ \t]*(?<text>.*?)(?:\n\n|\z)'#
const HEADER = r#'^[a-z]+(\([^)]*\))?!?:\s+'#
const REVERTS = r#'(?m)^This reverts commit (?<sha>[0-9a-f]{7,40})'#
const DOC_PATH = r#'(^|/)(docs?|tests?)/|\.md$|remaining-work|\.svg$|\.png$|\.mmd$'#
const HERE = (path self | path dirname)
const VERSION = r#'^v?(?<major>\d+)\.(?<minor>\d+)\.(?<patch>\d+)$'#

# ── Versions ────────────────────────────────────────────────────────────────

export def parse_version [text: string] {
    let m = ($text | str trim | parse --regex $VERSION)
    if ($m | is-empty) { error make --unspanned { msg: $"'($text)' is not a version of the form X.Y.Z" } }
    let g = ($m | first)
    { major: ($g.major | into int), minor: ($g.minor | into int), patch: ($g.patch | into int) }
}

def show [v: record] { $"($v.major).($v.minor).($v.patch)" }

# The version after `version` for a change of this significance; null when there is nothing to release.
export def bump [version: string, significance: string] {
    let v = (parse_version $version)
    let zero = ($v.major == 0)
    match $significance {
        "breaking" => (if $zero { show { major: 0, minor: ($v.minor + 1), patch: 0 } } else { show { major: ($v.major + 1), minor: 0, patch: 0 } })
        "feature" => (if $zero { show ($v | update patch ($v.patch + 1)) } else { show { major: $v.major, minor: ($v.minor + 1), patch: 0 } })
        "fix" => (show ($v | update patch ($v.patch + 1)))
        _ => null
    }
}

const ORDER = ["none" "fix" "feature" "breaking"]

def rank [significance: string] { $ORDER | enumerate | where item == $significance | first | get index }

# The highest significance of a list (none for an empty one).
export def highest [significances: list<string>] {
    $significances | reduce --fold "none" {|s, acc| if (rank $s) > (rank $acc) { $s } else { $acc } }
}

# ── One commit ──────────────────────────────────────────────────────────────

# A conventional header: { type, scope, bang }, or null.
export def parse_conventional [subject: string] {
    let m = ($subject | parse --regex $CONVENTIONAL)
    if ($m | is-empty) { return null }
    let g = ($m | first)
    { type: $g.type, scope: ($g.scope | default "" | str replace --regex '^\(|\)$' '' --all), bang: ($g.bang | default "" | is-not-empty) }
}

# Does the message carry a BREAKING CHANGE footer?
export def has_breaking_footer [body: string] {
    ($body | parse --regex $BREAKING_FOOTER) | is-not-empty
}

# What a BREAKING CHANGE footer says, on one line; "" when there is none.
export def breaking_note [body: string] {
    let m = ($body | parse --regex $BREAKING_NOTE)
    if ($m | is-empty) { "" } else { $m | first | get text | str replace --all --regex '\s+' ' ' | str trim }
}

# A subject without its `type(scope)!:` prefix.
export def description_of [subject: string] {
    $subject | str trim | str replace --regex $HEADER ''
}

# Does a path ship nothing (docs and tests)?
def is_docs_path [file: string] { $file =~ $DOC_PATH }

# What a commit message that is not conventional probably is, from its leading verb and the files it touches:
# { type, how } where how is "inferred" or "unclassified". It cannot say a commit is breaking.
export def infer [subject: string, files: list<string>] {
    let l = ($subject | str trim | str lowercase)
    let ships = ($files | any {|f| not (is_docs_path $f) })
    if ($l =~ '^revert\b') { return { type: "revert", how: "inferred" } }
    if ($l =~ '\b(fix|fixes|fixed|fixing|bug|bugfix|hotfix|resolve|resolves|resolved|correct|corrected|corrects|patch)\b') { return { type: "fix", how: "inferred" } }
    if ($l =~ '^(add|added|adds|adding|implement|implemented|implements|introduce|introduced|introduces|create|created|creates|support|new|feature|enable|enabled|enables)\b') {
        return (if $ships { { type: "feat", how: "inferred" } } else { { type: "docs", how: "inferred" } })
    }
    if ($l =~ '^(doc|docs|document|readme|update (the )?(docs|readme))') { return { type: "docs", how: "inferred" } }
    if ($l =~ '^(bump|update|upgrade)\b.*(dep|dependenc|lock|crate|version|nixpkgs|flake)') { return { type: "chore", how: "inferred" } }
    if ($l =~ '^(refactor|rename|move|cleanup|clean up|remove|delete|drop|simplif|tidy|format|lint|reformat)') { return { type: "refactor", how: "inferred" } }
    if $ships { { type: "fix", how: "unclassified" } } else { { type: "docs", how: "unclassified" } }
}

def significance_of [type: string, breaking: bool] {
    if $breaking { return "breaking" }
    match $type {
        "feat" => "feature"
        "fix" | "perf" | "revert" => "fix"
        _ => "none"
    }
}

# How one commit counts: { sha, subject, kind, type, scope, description, breaking, breaking_note, significance, note }. A commit is
# { sha, author, subject, body, files }. `kind` is conventional, inferred, unclassified or override.
export def classify [c: record, overrides: list = []] {
    let conventional = (parse_conventional $c.subject)
    let footer = (has_breaking_footer $c.body)
    let base = if $conventional != null {
        { kind: "conventional", type: $conventional.type, breaking: ($conventional.bang or $footer) }
    } else {
        let i = (infer $c.subject $c.files)
        { kind: $i.how, type: $i.type, breaking: $footer }
    }
    let hits = ($overrides | where {|o| ($o.sha | str length) >= 7 and ($c.sha | str starts-with $o.sha) })
    let footer_note = (breaking_note $c.body)
    let fin = if ($hits | is-empty) {
        $base | insert note "" | insert breaking_note $footer_note
    } else {
        let o = ($hits | first)
        if ($o.ignore? | default false) {
            { kind: "override", type: "chore", breaking: false, note: $"ignored: ($o.reason)", breaking_note: "" }
        } else {
            # What to do about a break, when the commit's message cannot say it: the override's own, else its footer's.
            { kind: "override", type: ($o.type? | default $base.type), breaking: ($o.breaking? | default $base.breaking), note: $o.reason, breaking_note: ($o.breaking_note? | default $footer_note) }
        }
    }
    {
        sha: $c.sha
        subject: $c.subject
        kind: $fin.kind
        type: $fin.type
        scope: (if $conventional != null { $conventional.scope } else { "" })
        description: (description_of $c.subject)
        breaking: $fin.breaking
        breaking_note: $fin.breaking_note
        significance: (significance_of $fin.type $fin.breaking)
        note: $fin.note
    }
}

# A revert and the commit it reverts, both in the range, change nothing together: both stop counting.
export def cancel_reverts [classified: list, commits: list] {
    let shas = ($classified | get sha)
    let pairs = ($commits | each {|c|
        let m = ($c.body | parse --regex $REVERTS)
        if ($m | is-empty) { null } else {
            let target = ($shas | where {|s| $s | str starts-with ($m | first | get sha) } | first | default null)
            if $target == null { null } else { { revert: $c.sha, target: $target } }
        }
    } | compact)
    let cancelled = ($pairs | each {|p| [$p.revert $p.target] } | flatten)
    $classified | each {|c|
        if $c.sha in $cancelled {
            $c | update significance "none" | update kind "cancelled" | update note "reverted within the range"
        } else { $c }
    }
}

# ── The suggestion ──────────────────────────────────────────────────────────

# What the commits since `previous` point to: the next version, why, and what to look at.
export def suggest_version [previous: string, commits: list, overrides: list = []] {
    let classified = (cancel_reverts ($commits | each {|c| classify $c $overrides }) $commits)
    let significance = (highest ($classified | get significance))
    {
        previous: $previous
        next: (bump $previous $significance)
        significance: $significance
        commits: ($classified | length)
        entries: $classified
        by_significance: ($ORDER | reverse | each {|s| { significance: $s, count: ($classified | where significance == $s | length) } })
        breaking: ($classified | where significance == "breaking" | select sha subject kind note)
        inferred: ($classified | where kind == "inferred" and significance != "none" | select sha subject type)
        unclassified: ($classified | where kind == "unclassified" | select sha subject)
        overridden: ($classified | where kind == "override" | select sha subject note)
    }
}

# ── The repository ──────────────────────────────────────────────────────────

# The non-merge commits in from..to, oldest last as git prints them.
export def commits_between [from: string, to: string = "HEAD"] {
    let fmt = "%x1e%H%x1f%an%x1f%s%x1f%b%x1f"
    let out = (do { ^git log --no-merges --name-only $"--format=($fmt)" $"($from)..($to)" } | complete)
    if $out.exit_code != 0 { error make --unspanned { msg: $"git log ($from)..($to) failed: ($out.stderr | str trim)" } }
    $out.stdout | split row (char --integer 30) | where {|b| ($b | str trim | is-not-empty) } | each {|b|
        let p = ($b | split row (char --integer 31))
        {
            sha: ($p | get 0 | str trim)
            author: ($p | get 1)
            subject: ($p | get 2)
            body: ($p | get 3)
            files: (($p | get 4? | default "") | lines | where {|f| ($f | str trim | is-not-empty) })
        }
    }
}

# The previous release: the latest vX.Y.Z tag reachable from HEAD, else the baseline.
export def previous_release [] {
    let tag = (do { ^git describe --tags --abbrev=0 --match "v[0-9]*.[0-9]*.[0-9]*" HEAD } | complete)
    if $tag.exit_code == 0 {
        let t = ($tag.stdout | str trim)
        { version: ($t | str replace --regex '^v' ''), rev: $t, from: "a release tag" }
    } else {
        let base = (open ($HERE | path join "release-baseline.nuon"))
        { version: $base.version, rev: $base.commit, from: "the baseline (no release tag yet)" }
    }
}

def overrides_file [] { $HERE | path join "release-overrides.nuon" }

# The evidence and the suggestion as text, for a person to read (`suggest`) and for the release to show before it asks.
export def suggest_report [s: record, prev: record, rev: string] {
    mut lines = [
        $"previous:    v($s.previous), from ($prev.from)"
        $"commits:     ($s.commits) since ($rev | str substring 0..7)"
    ]
    for r in $s.by_significance { $lines = ($lines | append $"             ($r.count | into string | fill --width 4 --alignment right) ($r.significance)") }
    if ($s.breaking | is-not-empty) {
        $lines = ($lines | append "breaking:")
        for b in $s.breaking { $lines = ($lines | append $"  ($b.sha | str substring 0..7)  ($b.subject | str substring 0..90)") }
    }
    if ($s.inferred | is-not-empty) { $lines = ($lines | append $"inferred from the wording, not written as conventional commits: ($s.inferred | length) \(feature or fix; never breaking\)") }
    if ($s.unclassified | is-not-empty) { $lines = ($lines | append $"could not be classified, counted as patches because they touch code: ($s.unclassified | length)") }
    if ($s.overridden | is-not-empty) { $lines = ($lines | append $"overridden in release-overrides.nuon: ($s.overridden | length)") }
    $lines = ($lines | append (if $s.next == null {
        "suggested:   nothing to release (only docs, tests and housekeeping)"
    } else {
        $"suggested:   v($s.next)  \(($s.significance)\)"
    }))
    $lines | str join "\n"
}

# The suggestion for the commits since the previous release (or `from`), with what it was calculated from.
export def current_suggestion [from: string = ""] {
    let prev = (previous_release)
    let rev = (if ($from | is-empty) { $prev.rev } else { $from })
    let commits = (commits_between $rev)
    let s = (suggest_version $prev.version $commits (open (overrides_file)))
    { suggestion: $s, previous: $prev, rev: $rev }
}

def "main suggest" [
    --from: string = ""          # count from this commit or tag instead of the previous release
] {
    let c = (current_suggestion $from)
    print (suggest_report $c.suggestion $c.previous $c.rev)
}

def main [] {
    print "usage: nu infra/semver.nu suggest [--from REV]"
}
