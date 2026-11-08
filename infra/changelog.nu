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

# infra/changelog.nu
#
# CHANGELOG.md, generated at a release from the same classification of commits that chooses the version (infra/semver.nu), so
# the two cannot disagree. How it looks is infra/changelog.nuon.
#
#   nu infra/changelog.nu preview [--version V] [--highlights TEXT]   print the section for the next release
#   nu infra/changelog.nu write   [--version V] [--highlights TEXT]   put it in CHANGELOG.md
#
# A section has, in this order and only when there is something to say: the commit the release's images were built from (the
# `chore(release): prepare` commit: the images carry the version and never the commit, infra/INFRA.md, "The release"), the
# highlights (written by a person at the release),
# breaking changes with what each one says to do, the images that changed with their digests (from the lock), the features,
# the fixes and the performance changes grouped by scope, a sentence that counts what is not listed (documentation,
# housekeeping, refactoring, tests), and a collapsed list of the changes that were not written as conventional commits. A
# commit whose type was inferred from its wording carries a dagger. A revert and what it reverts do not appear.
#
# Writing replaces the section of the same version, so running a release again changes nothing else; older sections are history
# and are left exactly as they are.

use semver.nu [commits_between, previous_release, suggest_version]
use lock.nu

const HERE = (path self | path dirname)
const SELF = (path self)

# ── Pieces ──────────────────────────────────────────────────────────────────

# The license banner of this file, as the HTML comment the other Markdown files start with.
export def md_banner [] {
    let lines = (open --raw $SELF | lines)
    let start = ($lines | enumerate | where {|r| $r.item | str starts-with "# ACE Tool" } | first | get index)
    let end = ($lines | enumerate | where {|r| $r.item | str contains "DM26-0670" } | first | get index)
    let body = ($lines | skip $start | first ($end - $start + 1)
        | each {|l| let t = ($l | str replace --regex '^#\s?' ''); if ($t | is-empty) { "" } else { $"  ($t)" } })
    ["<!--"] | append $body | append "-->" | str join "\n"
}

def sentence [text: string] {
    let t = ($text | str trim | str replace --regex '\.$' '')
    if ($t | is-empty) { $t } else { $"($t | str substring 0..0 | str uppercase)($t | str substring 1..)" }
}

def with_emoji [emoji: string, title: string] {
    if ($emoji | is-empty) { $title } else { $"($emoji) ($title)" }
}

def short [digest: string] { $"($digest | str replace 'sha256:' '' | str substring 0..11)" }

# What changed in the lock: { image, tag, before, after, change } for an image that is new, changed or gone (not for one that
# is the same), for the architecture the cluster runs.
export def image_changes [before: list, after: list, arch: string] {
    let b = ($before | where arch == $arch)
    let a = ($after | where arch == $arch)
    let now = ($a | each {|r|
        let old = ($b | where {|x| $x.image == $r.image and $x.tag == $r.tag })
        if ($old | is-empty) {
            { image: $r.image, tag: $r.tag, before: "", after: $r.digest, change: "new" }
        } else if ($old | first | get digest) != $r.digest {
            { image: $r.image, tag: $r.tag, before: ($old | first | get digest), after: $r.digest, change: "changed" }
        } else { null }
    } | compact)
    let gone = ($b | where {|x| ($a | where {|r| $r.image == $x.image and $r.tag == $x.tag } | is-empty) }
        | each {|r| { image: $r.image, tag: $r.tag, before: $r.digest, after: "", change: "removed" } })
    $now | append $gone | sort-by image tag
}

def images_table [changes: list] {
    let rows = ($changes | each {|c|
        let what = match $c.change {
            "new" => $"new, `sha256:(short $c.after)`"
            "removed" => $"removed, was `sha256:(short $c.before)`"
            _ => $"`sha256:(short $c.before)` -> `sha256:(short $c.after)`"
        }
        $"| `($c.image):($c.tag)` | ($what) |"
    })
    ["| Image | Change |" "|---|---|"] | append $rows | str join "\n"
}

# A scope can name several, `a,b`: the first groups the entry and the others are mentioned in it.
def primary_scope [scope: string] { $scope | split row "," | first | str trim }
def also_scopes [scope: string] { $scope | split row "," | skip 1 | each {|s| $s | str trim } | where {|s| $s != "" } }

# One entry: the sentence, the other scopes it names, and the dagger for a type that was inferred.
def entry_text [e: record] {
    let mark = if $e.kind == "inferred" { " †" } else { "" }
    let also = if ($e.also | is-empty) { "" } else { $" \(also: ($e.also | str join ', ')\)" }
    $"(sentence $e.description)($also)($mark)"
}

# Entries grouped by scope, scopes in order, the commits without one last; each scope a bold line and its bullets.
def by_scope [entries: list] {
    let scoped = ($entries | where primary != "" | group-by primary)
    let names = ($scoped | columns | sort)
    let blocks = ($names | each {|n| [$"**($n)**" ""] | append ($scoped | get $n | each {|e| $"- (entry_text $e)" }) | str join "\n" })
    let rest = ($entries | where primary == "")
    let all = if ($rest | is-empty) { $blocks } else {
        $blocks | append ([ "**Other**" "" ] | append ($rest | each {|e| $"- (entry_text $e)" }) | str join "\n")
    }
    $all | str join "\n\n"
}

# A section that lists entries: grouped by scope, or, when there are more than `fold_over`, a count for each scope and the
# entries themselves in a collapsed list.
def group_body [entries: list, fold_over: int] {
    if ($entries | length) <= $fold_over { return (by_scope $entries) }
    let scoped = ($entries | where primary != "" | group-by primary)
    let counts = ($scoped | columns | sort | each {|n| $"- **($n)**: ($scoped | get $n | length)" })
    let counts = if ($entries | where primary == "" | is-empty) { $counts } else { $counts | append $"- **Other**: ($entries | where primary == '' | length)" }
    [ ($counts | str join "\n") "" $"<details><summary>All ($entries | length) entries</summary>" "" (by_scope $entries) "" "</details>" ] | str join "\n"
}

def breaking_body [entries: list] {
    $entries | each {|e|
        let scope = if $e.scope == "" { "" } else { $"*\(($e.scope)\)* " }
        let note = if ($e.breaking_note | is-empty) { "" } else { $"\n  ($e.breaking_note)" }
        $"- ($scope)(sentence $e.description)($note)"
    } | str join "\n"
}

# "164 documentation, 77 housekeeping and 21 test commits" for what the changelog does not list.
def folded_sentence [entries: list, folded: record] {
    let counted = ($entries | where {|e| $e.significance == "none" and $e.kind != "cancelled" })
    let parts = ($folded | columns | each {|t|
        let n = ($counted | where type == $t | length)
        if $n == 0 { null } else { { n: $n, label: ($folded | get $t) } }
    } | compact | sort-by n --reverse | each {|p| $"($p.n) ($p.label)" })
    if ($parts | is-empty) { return "" }
    let list = if ($parts | length) == 1 { $parts | first } else {
        $"(($parts | drop 1) | str join ', ') and ($parts | last)"
    }
    let total = ($counted | length)
    $"Also in this release, not listed: ($list) (if $total == 1 { 'commit' } else { 'commits' })."
}

# ── A release's section ─────────────────────────────────────────────────────

# release: { version, date, previous, highlights, built_from? }; entries: what `suggest_version` returned as `entries`; images:
# image_changes. `built_from` is the commit the images were built from, when they were built for the release.
export def render_section [release: record, entries: list, images: list, config: record] {
    let live = ($entries | where kind != "cancelled" | each {|e| $e | merge { primary: (primary_scope $e.scope), also: (also_scopes $e.scope) } })
    let breaking = ($live | where significance == "breaking")
    let features = ($live | where {|e| $e.significance == "feature" and $e.kind != "unclassified" })
    let perf = ($live | where {|e| $e.significance == "fix" and $e.type == "perf" and $e.kind != "unclassified" })
    let fixes = ($live | where {|e| $e.significance == "fix" and $e.type != "perf" and $e.kind != "unclassified" })
    let unclassified = ($live | where kind == "unclassified" and significance != "none")

    let head = $"## [($release.version)] - ($release.date)"
    let link = if ($config.repository | is-empty) or ($release.previous | is-empty) { "" } else {
        $"Full diff: ($config.repository)/compare/v($release.previous)...v($release.version)"
    }
    let built = ($release.built_from? | default "")
    let built_line = if ($built | is-empty) { "" } else {
        $"Images built from `($built | str substring 0..11)`, the commit that set VERSION to ($release.version)."
    }
    let highlights = if ($release.highlights | str trim | is-empty) { "" } else {
        $release.highlights | str trim | lines | each {|l| $"> ($l)" } | str join "\n"
    }

    let blocks = ($config.groups | each {|g|
        let body = match $g.source {
            "breaking" => (if ($breaking | is-empty) { "" } else { breaking_body $breaking })
            "images" => (if ($images | is-empty) { "" } else { images_table $images })
            "feature" => (if ($features | is-empty) { "" } else { group_body $features $config.fold_over })
            "fix" => (if ($fixes | is-empty) { "" } else { group_body $fixes $config.fold_over })
            "perf" => (if ($perf | is-empty) { "" } else { group_body $perf $config.fold_over })
            _ => ""
        }
        if ($body | is-empty) { null } else { $"### (with_emoji $g.emoji $g.title)\n\n($body)" }
    } | compact)

    let inferred_note = if ($live | where kind == "inferred" | is-empty) { "" } else {
        "† Classified from its wording: the commit was not written as a conventional commit."
    }
    let folded = (folded_sentence $live $config.folded)
    let unclass = if ($unclassified | is-empty) { "" } else {
        let items = ($unclassified | each {|e| $"- ($e.subject | str trim) \(`($e.sha | str substring 0..6)`\)" } | str join "\n")
        let n = ($unclassified | length)
        let what = if $n == 1 { "1 change was not written as a conventional commit and is counted as a patch" } else { $"($n) changes were not written as conventional commits and are counted as patches" }
        $"<details><summary>($what)</summary>\n\n($items)\n\n</details>"
    }

    [$head $link $built_line $highlights] | append $blocks | append [$inferred_note $folded $unclass]
        | where {|b| $b | is-not-empty } | str join "\n\n"
}

# ── The file ────────────────────────────────────────────────────────────────

export def file_header [config: record] {
    $"(md_banner)\n\n# ($config.title)\n\n($config.intro)"
}

# The text before the first release section, and each section with its version.
export def split_file [text: string] {
    let lines = ($text | lines)
    let marks = ($lines | enumerate | where {|r| $r.item =~ '^## \[' } | get index)
    if ($marks | is-empty) { return { header: ($text | str trim --right), sections: [] } }
    let header = ($lines | first ($marks | first) | str join "\n" | str trim --right)
    let ends = ($marks | skip 1 | append ($lines | length))
    let sections = ($marks | zip $ends | each {|p|
        let chunk = ($lines | skip $p.0 | first ($p.1 - $p.0) | str join "\n" | str trim --right)
        let v = ($chunk | lines | first | parse --regex '^## \[(?<v>[^\]]+)\]' | first | get v)
        { version: $v, text: $chunk }
    })
    { header: $header, sections: $sections }
}

# The file with `section` for `version`: replacing that version's section if the file has one, else the newest at the top.
export def with_section [existing: string, version: string, section: string, config: record] {
    let parts = if ($existing | str trim | is-empty) { { header: (file_header $config), sections: [] } } else { split_file $existing }
    let has = ($parts.sections | where version == $version | is-not-empty)
    let sections = if $has {
        $parts.sections | each {|s| if $s.version == $version { { version: $version, text: ($section | str trim --right) } } else { $s } }
    } else {
        [{ version: $version, text: ($section | str trim --right) }] | append $parts.sections
    }
    $"($parts.header)\n\n($sections | get text | str join "\n\n")\n"
}

# ── The repository ──────────────────────────────────────────────────────────

def config_file [] { open ($HERE | path join "changelog.nuon") }

def previous_lock [prev: record] {
    if $prev.from != "a release tag" { return [] }
    let r = (do { ^git show $"($prev.rev):infra/images.lock.nuon" } | complete)
    if $r.exit_code == 0 { $r.stdout | from nuon } else { [] }
}

def build [version: string, highlights: string, date: string, from: string, built_from: string = ""] {
    let prev = (previous_release)
    let rev = (if ($from | is-empty) { $prev.rev } else { $from })
    let commits = (commits_between $rev)
    let overrides = (open ($HERE | path join "release-overrides.nuon"))
    let s = (suggest_version $prev.version $commits $overrides)
    let v = (if ($version | is-empty) { $s.next } else { $version })
    if $v == null { error make --unspanned { msg: "nothing to release: only docs, tests and housekeeping since the previous release" } }
    let cluster = (open ($HERE | path join "config" "cluster.nuon"))
    let images = (image_changes (previous_lock $prev) (lock effective) $cluster.arch)
    let release = { version: $v, date: $date, previous: (if $prev.from == "a release tag" { $prev.version } else { "" }), highlights: $highlights, built_from: $built_from }
    { version: $v, section: (render_section $release ($s.entries | reverse) $images (config_file)) }
}

def today [] { date now | format date "%Y-%m-%d" }

def "main preview" [
    --version: string = ""       # the version (default: what infra/semver.nu suggests)
    --highlights: string = ""    # a few sentences for the top of the section
    --date: string = ""
    --from: string = ""          # count from this commit or tag instead of the previous release
] {
    let b = (build $version $highlights (if ($date | is-empty) { today } else { $date }) $from)
    print $b.section
}

def "main write" [
    --version: string = ""
    --highlights: string = ""
    --date: string = ""
    --from: string = ""
    --built-from: string = ""    # the commit the release's images were built from
] {
    let b = (build $version $highlights (if ($date | is-empty) { today } else { $date }) $from $built_from)
    let root = (^git rev-parse --show-toplevel | str trim)
    let file = ($root | path join "CHANGELOG.md")
    let existing = (if ($file | path exists) { open --raw $file } else { "" })
    with_section $existing $b.version $b.section (config_file) | save --force $file
    print $"wrote the section for ($b.version) to ($file)"
}

def main [] {
    print "usage: nu infra/changelog.nu preview|write [--version V] [--highlights TEXT] [--date D] [--from REV]"
}
