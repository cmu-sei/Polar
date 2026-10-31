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

# infra/release.nu
#
# A release of the platform: one version for every Polar image, chosen by the evidence of the commits since the last one, made of
# the images in one coherent build, written down in the lock and the changelog, and tagged.
#
#   nu infra/release.nu plan                          what the next release would be; changes nothing
#   nu infra/release.nu gates [--only a,b]            run the gates (infra/release.nuon) and say which failed
#   nu infra/release.nu run [--version V] [--highlights TEXT] [--dry-run] [--yes]
#                           [--skip-gate NAME] [--skip-images] [--allow-dirty]
#   nu infra/release.nu secrets                       the findings of the secret scan, by id (never the values)
#   nu infra/release.nu secrets accept ID... --reason TEXT | --placeholder-like
#
# `run`, in order: it checks the tree and the branch; runs the gates and stops on a blocking failure; shows the evidence and asks for the
# version (the one infra/semver.nu suggests is the default); asks for the highlights; writes the version into VERSION and commits
# `chore(release): prepare vX.Y.Z`, so the images are built from a commit whose VERSION is the release; builds and pushes every image
# once and makes their digests the committed lock; writes the changelog section; and, once you say so, commits `chore(release): vX.Y.Z`
# and tags it, then writes `X.Y.Z+dev` into VERSION and commits `chore: X.Y.Z+dev`. The tagged commit differs from the prepare commit
# only in the lock and the changelog, which no image is built from, so building the tag gives the locked digests again. It never
# pushes. `--dry-run` does everything up to the point of writing and says what it would have written and committed.

use semver.nu [current_suggestion, parse_version, suggest_report]
use lock.nu

const HERE = (path self | path dirname)

# ── Pure parts ──────────────────────────────────────────────────────────────

# -1, 0 or 1 as version `a` is before, the same as, or after `b`.
export def compare_versions [a: string, b: string] {
    let x = (parse_version $a)
    let y = (parse_version $b)
    for k in [major minor patch] {
        if ($x | get $k) < ($y | get $k) { return (-1) }
        if ($x | get $k) > ($y | get $k) { return 1 }
    }
    0
}

# What is wrong with a version someone chose, given the previous release.
export def version_problems [chosen: string, previous: string, existing_tags: list<string> = []] {
    let parsed = (try { parse_version $chosen; true } catch { false })
    if not $parsed { return [$"'($chosen)' is not a version of the form X.Y.Z"] }
    mut problems = []
    if (compare_versions $chosen $previous) <= 0 {
        $problems = ($problems | append $"v($chosen) is not after the previous release, v($previous)")
    }
    if $"v($chosen)" in $existing_tags {
        $problems = ($problems | append $"the tag v($chosen) already exists")
    }
    $problems
}

# What is wrong with the repository before a release: { dirty, branch, behind_upstream }.
export def preflight_problems [state: record, allow_dirty: bool] {
    mut problems = []
    if $state.dirty and not $allow_dirty {
        $problems = ($problems | append "the working tree has uncommitted changes to tracked files: commit or stash them, or pass --allow-dirty (the release commit would not contain them)")
    }
    if ($state.branch | is-empty) {
        $problems = ($problems | append "HEAD is detached: check out the branch the release is made on")
    }
    $problems
}

# The gates a run will use: those named by `only`, else all, without those in `skip`. Unknown names are an error.
export def select_gates [config: record, only: list<string>, skip: list<string>] {
    let known = ($config.gates | get name)
    let unknown = ($only ++ $skip | where {|n| $n not-in $known })
    if ($unknown | is-not-empty) { error make --unspanned { msg: $"unknown gate\(s\): ($unknown | str join ', '). Known: ($known | str join ', ')" } }
    let chosen = if ($only | is-empty) { $config.gates } else { $config.gates | where {|g| $g.name in $only } }
    { run: ($chosen | where {|g| $g.name not-in $skip }), skipped: ($chosen | where {|g| $g.name in $skip } | get name) }
}

# The findings of a noseyparker report (`report --format json`) as rows that never hold the matched text:
# { id, rule, commit, path, count, placeholder_like }.
export def secret_rows [report: list] {
    $report | each {|f|
        let m = ($f.matches | first)
        let pv = (($m.provenance? | default []) | first | default {})
        let fc = ($pv.first_commit? | default {})
        let matched = (match_text $m)
        {
            id: $f.finding_id
            rule: $f.rule_name
            commit: ((($fc.commit_metadata? | default {}).commit_id? | default "") | str substring 0..6)
            path: ($fc.blob_path? | default ($pv.path? | default ""))
            count: $f.num_matches
            placeholder_like: (looks_like_placeholder $matched)
        }
    } | sort-by rule path
}

def match_text [m: record] {
    let s = (($m.snippet? | default {}).matching? | default "")
    if ($s | describe) == "string" { $s } else { try { $s | into binary | decode utf-8 } catch { "" } }
}

# A value that is plainly an example, from what it is made of; a rough test, used only to help a person choose what to accept.
export def looks_like_placeholder [text: string] {
    if ($text | is-empty) { return false }
    let low = ($text | str lowercase)
    let words = ["example" "placeholder" "xxxx" "changeme" "your" "dummy" "fake" "test" "password" "secret" "token" "abc123" "hunter"]
    if ($words | any {|w| $low | str contains $w }) { return true }
    let core = ($text | str replace --regex '^(ghp_|glpat-)' '')
    let chars = ($core | split chars)
    let top = ($chars | uniq --count | sort-by count --reverse | first | get count)
    ($top / ($chars | length)) > 0.45
}

# The findings not covered by the baseline (an entry matches an id by its start, at least 8 characters).
export def unaccepted [rows: list, baseline: list] {
    $rows | where {|r| not ($baseline | any {|b| ($b.id | str length) >= 8 and ($r.id | str starts-with $b.id) }) }
}

export def release_message [version: string] { $"chore\(release\): v($version)" }

# ── The platform version file ───────────────────────────────────────────────
# VERSION, at the repository's root, is the platform version every binary embeds (src/agents/polar-version) and every image carries as
# org.opencontainers.image.version: `X.Y.Z` on a release's commits, `X.Y.Z+dev` between releases (semver build metadata, so a
# development build never passes for a release).

const PLATFORM_VERSION = r#'^(?<core>(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*))(?<dev>\+dev)?$'#

# The file VERSION's text: { version, release, development }, where `release` is X.Y.Z. An error for anything else.
export def read_version_text [text: string] {
    let t = ($text | str trim)
    let m = ($t | parse --regex $PLATFORM_VERSION)
    if ($m | is-empty) { error make --unspanned { msg: $"VERSION holds '($t)', which is not X.Y.Z or X.Y.Z+dev" } }
    let g = ($m | first)
    { version: $t, release: $g.core, development: ($g.dev | default "" | is-not-empty) }
}

# What VERSION holds for `version`: the version and a newline.
export def version_file_text [version: string] {
    read_version_text $version | ignore
    $"($version)\n"
}

# What VERSION reads between a release and the next: the release with `+dev`.
export def dev_version [release: string] {
    let v = (read_version_text $release)
    if $v.development { error make --unspanned { msg: $"($release) is already a development version" } }
    $"($v.release)+dev"
}

# The commits a release of `version` makes, in order: what each writes into VERSION (null: it leaves VERSION alone), the files it
# commits, its message and its tag. Only the prepare commit is built from: the release commit holds what the build wrote.
export def release_steps [version: string] {
    read_version_text $version | ignore
    [
        { step: "prepare", version_file: $version, files: ["VERSION"], message: $"chore\(release\): prepare v($version)", tag: null }
        { step: "release", version_file: null, files: ["infra/images.lock.nuon" "CHANGELOG.md"], message: (release_message $version), tag: $"v($version)" }
        { step: "development", version_file: (dev_version $version), files: ["VERSION"], message: $"chore: (dev_version $version)", tag: null }
    ]
}

# Whether VERSION agrees with the previous release `last`: it does when it reads `last+dev` (between releases) or `last` itself (the
# release's own commits). { version, agrees, note }.
export def version_file_status [text: string, last: string] {
    let v = (try { read_version_text $text } catch {|e| { problem: $e.msg } })
    if ($v.problem? != null) {
        { version: ($text | str trim), agrees: false, note: $v.problem }
    } else if $v.release == $last and $v.development {
        { version: $v.version, agrees: true, note: $"agrees with v($last): development since that release" }
    } else if $v.release == $last {
        { version: $v.version, agrees: true, note: $"agrees with v($last): the release's own version, as on its prepare and release commits" }
    } else {
        { version: $v.version, agrees: false, note: $"does not agree with v($last): between releases it reads ($last)+dev" }
    }
}

# What is wrong with the release commit, given the files it changed from the prepare commit: anything but what the release step
# commits is an input to an image, or could be, and building the tag would not give the locked digests.
export def release_diff_problems [changed: list<string>, version: string] {
    let allowed = (release_steps $version | where step == "release" | first | get files)
    $changed | where {|f| $f not-in $allowed } | each {|f| $"the release commit changes ($f), which is not one of ($allowed | str join ', ')" }
}

# What a dry run says the release would do, a line each.
export def release_plan_lines [version: string, skip_images: bool] {
    let s = (release_steps $version)
    let prepare = ($s | where step == "prepare" | first)
    let release = ($s | where step == "release" | first)
    let dev = ($s | where step == "development" | first)
    [
        $"    - write ($prepare.version_file) into VERSION and commit `($prepare.message)` \(VERSION alone\): the images are built from this commit"
        (if $skip_images { "    - make the lock from the digests already recorded \(--skip-images: they were not built from the prepare commit, and the lock does not say they were\)" } else { "    - move this machine's lock aside, build and push every image once, and make their digests the lock, with the prepare commit as what they were built from" })
        "    - write the changelog section:"
        $"    - commit `($release.message)` with ($release.files | str join ' and '), check it changes nothing else since the prepare commit, and tag ($release.tag)"
        $"    - write ($dev.version_file) into VERSION and commit `($dev.message)`"
        "    - push nothing"
    ]
}

# ── The gates ───────────────────────────────────────────────────────────────

def timed [body: closure] {
    let start = (date now)
    let r = (do $body)
    $r | merge { seconds: ((((date now) - $start) / 1sec) | math round) }
}

def config_file [] { open ($HERE | path join "release.nuon") }

def tests_to_run [config: record] {
    glob ($HERE | path join "tests" "*.nu") | where {|f| ($f | path parse | get stem) not-in $config.heavy_tests } | sort
}

def run_nu [args: list<string>] {
    let r = (do { ^nu ...$args } | complete)
    { ok: ($r.exit_code == 0), detail: (if $r.exit_code == 0 { "" } else { ($r.stdout + $r.stderr | lines | last 6 | str join " | ") }) }
}

def scan_published [] {
    let root = (^git rev-parse --show-toplevel | str trim)
    let work = (^mktemp -d | str trim)
    # `--no-local`: a clone of a local path hard-links the whole object store, unreachable commits included. This one carries only
    # what the refs reach, which is what has been, or will be, published.
    let cloned = (do { ^git clone --quiet --bare --no-local $root ($work | path join "repo.git") } | complete)
    if $cloned.exit_code != 0 { error make --unspanned { msg: $"could not clone the repository to scan it: ($cloned.stderr | str trim)" } }
    let ds = ($work | path join "ds")
    let scan = (do { ^noseyparker-cli scan --datastore $ds ($work | path join "repo.git") } | complete)
    if $scan.exit_code != 0 { error make --unspanned { msg: $"noseyparker scan failed: ($scan.stderr | lines | last 3 | str join ' | ')" } }
    let report = (^noseyparker-cli report --datastore $ds --format json | from json)
    ^rm -rf $work
    secret_rows $report
}

# Runs one tool of the static analysis runner; where it fails, the detail is the last line of its own output and where the rest is.
def run_static [tool: string, file: string] {
    let out = ($env.HOME | path join ".cache" "polar-release" $tool)
    mkdir $out
    let r = (run_nu [($HERE | path dirname | path join "scripts" "pipeline" "static-tools.nu") --only $tool --output-path $out])
    if $r.ok { return $r }
    let last = (try { open --raw ($out | path join $file) | ansi strip | lines | where {|l| ($l | str trim | is-not-empty) } | last } catch { "" })
    { ok: false, detail: $"($last | str trim); the whole output is ($out | path join $file)" }
}

def baseline [] { open ($HERE | path join "release-secrets-baseline.nuon") }

def run_gate [name: string, config: record] {
    match $name {
        "unit" => {
            let failed = (tests_to_run $config | each {|f| { test: ($f | path basename), r: (run_nu [$f]) } } | where {|x| not $x.r.ok })
            { ok: ($failed | is-empty), detail: ($failed | each {|x| $"($x.test): ($x.r.detail)" } | str join "; ") }
        }
        "deny" => (run_static "deny" "cargo_deny.txt")
        "clippy" => (run_static "clippy" "cargo_clippy.txt")
        "secrets" => {
            let open_ones = (unaccepted (scan_published) (baseline))
            {
                ok: ($open_ones | is-empty)
                detail: (if ($open_ones | is-empty) { "" } else { $"($open_ones | length) finding\(s\) not in the baseline; run: nu infra/release.nu secrets" })
            }
        }
        "resolve" => (run_nu [($HERE | path join "resolve.nu") check])
        "determinism" => {
            # The test renders on the cluster the tests are given, never the ambient KUBECONFIG, and skips without one
            # (infra/test-cluster.nu); a skip is no evidence, so without one the gate fails.
            if ($env.POLAR_TEST_CLUSTER_KUBECONFIG? | default "" | is-empty) {
                return { ok: false, detail: "it renders on a cluster: set POLAR_TEST_CLUSTER_KUBECONFIG to the kubeconfig of the one to use" }
            }
            let failed = ($config.determinism_targets | each {|t| { target: $t, r: (run_nu [($HERE | path join "tests" "render-determinism.nu") $t]) } } | where {|x| not $x.r.ok })
            { ok: ($failed | is-empty), detail: ($failed | each {|x| $"($x.target): ($x.r.detail)" } | str join "; ") }
        }
        _ => { ok: false, detail: $"no such gate: ($name)" }
    }
}

# Runs the gates and prints how each went. Returns { failed_blocking, results }.
def run_gates [config: record, only: list<string>, skip: list<string>] {
    let sel = (select_gates $config $only $skip)
    for n in $sel.skipped { print $"  skipped  ($n)  \(asked to\)" }
    mut results = []
    for g in $sel.run {
        print $"  running  ($g.name): ($g.what)"
        let r = (timed { run_gate $g.name $config })
        let mark = if $r.ok { "ok     " } else if $g.blocking { "FAILED " } else { "failed " }
        let why = if ($r.detail | is-empty) { "" } else { $": ($r.detail)" }
        print $"  ($mark) ($g.name)  \(($r.seconds)s)($why)"
        $results = ($results | append { name: $g.name, blocking: $g.blocking, ok: $r.ok, detail: $r.detail })
    }
    {
        failed_blocking: ($results | where {|r| $r.blocking and not $r.ok } | get name)
        failed_other: ($results | where {|r| not $r.blocking and not $r.ok } | get name)
        skipped: $sel.skipped
        results: $results
    }
}

# ── The repository ──────────────────────────────────────────────────────────

def repo_state [] {
    let status = (^git status --porcelain --untracked-files=no | lines | length)
    let branch = (^git branch --show-current | str trim)
    { dirty: ($status > 0), branch: $branch }
}

def existing_tags [] { ^git tag | lines }

def repo_root [] { ^git rev-parse --show-toplevel | str trim }

def version_file [] { repo_root | path join "VERSION" }

def head_sha [] { ^git rev-parse HEAD | str trim }

def head_subject [] { ^git log -1 --format=%s | str trim }

# The trailers a release's commits end with: whatever whoever cuts the release puts in POLAR_RELEASE_TRAILERS, one a line
# (an attribution, a sign-off); none when it is unset. The tool names no author of its own.
export def release_trailers [] {
    $env.POLAR_RELEASE_TRAILERS? | default "" | lines | each {|l| $l | str trim } | where {|l| $l | is-not-empty }
}

# Commits exactly `files` (whatever else is staged stays out) with `message`.
def commit_files [files: list<string>, message: string] {
    let root = (repo_root)
    ^git -C $root add ...$files
    let trailers = (release_trailers)
    let footer = if ($trailers | is-empty) { [] } else { ["-m" ($trailers | str join "\n")] }
    ^git -C $root commit -q -m $message ...$footer -- ...$files
}

# Writes what a step puts into VERSION and commits it.
def commit_version_step [step: record] {
    version_file_text $step.version_file | save --force (version_file)
    commit_files $step.files $step.message
}

def csv [text: string] { $text | split row "," | each {|t| $t | str trim } | where {|t| $t != "" } }

# ── Commands ────────────────────────────────────────────────────────────────

def "main plan" [] {
    let c = (current_suggestion)
    print (suggest_report $c.suggestion $c.previous $c.rev)
    let file = (version_file)
    let status = if ($file | path exists) { version_file_status (open --raw $file) $c.previous.version } else { { version: "(missing)", agrees: false, note: "there is no VERSION file at the repository's root" } }
    print $"VERSION:     ($status.version) \(($status.note)\)"
    let cfg = (config_file)
    print ""
    print "gates:"
    for g in $cfg.gates {
        let kind = if $g.blocking { "blocking" } else { "report-only" }
        print $"  ($g.name)  \(($kind)\)  ($g.what)"
    }
    let state = (repo_state)
    let problems = (preflight_problems $state false)
    print ""
    if ($problems | is-empty) { print "the repository is ready for a release" } else { for p in $problems { print $"not ready: ($p)" } }
}

def "main gates" [
    --only: string = ""          # comma-separated gate names (default: all)
    --skip-gate: string = ""     # comma-separated gates to leave out
] {
    let r = (run_gates (config_file) (csv $only) (csv $skip_gate))
    if ($r.failed_blocking | is-not-empty) {
        print -e $"blocking gates failed: ($r.failed_blocking | str join ', ')"
        exit 1
    }
    print "no blocking gate failed"
}

def "main secrets" [] {
    let rows = (scan_published)
    let open_ones = (unaccepted $rows (baseline))
    print $"findings in what the refs reach: ($rows | length); not in the baseline: ($open_ones | length)"
    for r in $open_ones {
        let mark = if $r.placeholder_like { "looks like an example" } else { "NOT obviously an example" }
        print $"  ($r.id | str substring 0..11)  ($r.rule)  ($r.commit)  ($r.path)  \(($mark))"
    }
    if ($open_ones | is-not-empty) {
        print ""
        print "accept the ones you have looked at: nu infra/release.nu secrets accept ID... --reason \"why\""
        print "or every one that looks like an example:  nu infra/release.nu secrets accept --placeholder-like --reason \"why\""
    }
}

def "main secrets accept" [
    ...ids: string               # finding ids, or their first 8 or more characters
    --reason: string = ""
    --placeholder-like           # every open finding that looks like an example value (you are saying you looked at the list)
] {
    if ($reason | is-empty) { error make --unspanned { msg: "say why: --reason TEXT" } }
    let open_ones = (unaccepted (scan_published) (baseline))
    let picked = if $placeholder_like {
        $open_ones | where placeholder_like
    } else {
        $open_ones | where {|r| $ids | any {|i| ($i | str length) >= 8 and ($r.id | str starts-with $i) } }
    }
    if ($picked | is-empty) { error make --unspanned { msg: "no open finding matches" } }
    let entries = ($picked | each {|r| { id: $r.id, reason: $reason } })
    let all = (baseline | append $entries)
    let text = $"(open --raw ($HERE | path join 'release-secrets-baseline.nuon') | lines | take while {|l| not ($l | str starts-with '[]') and not ($l | str starts-with '[') } | str join "\n")\n[\n($all | each {|e| $"  {id: \"($e.id)\", reason: \"($e.reason | str replace --all '\"' '')\"}" } | str join "\n")\n]\n"
    $text | save --force ($HERE | path join "release-secrets-baseline.nuon")
    print $"accepted ($entries | length) finding\(s\) in infra/release-secrets-baseline.nuon"
}

def "main run" [
    --version: string = ""       # the version (default: what the evidence suggests, asked for)
    --highlights: string = ""    # a few sentences for the top of the changelog section
    --dry-run                    # do everything but write, build or commit
    --yes                        # do not ask (the version must be given, or the suggestion is used)
    --skip-gate: string = ""     # comma-separated gates to leave out
    --skip-images                # do not build and push: make the lock from what is already there
    --allow-dirty
] {
    let cfg = (config_file)
    let problems = (preflight_problems (repo_state) $allow_dirty)
    if ($problems | is-not-empty) { for p in $problems { print -e $"  ERROR: ($p)" }; exit 1 }

    print "== gates"
    let skips = (csv $skip_gate)
    let before_cfg = ($cfg | update gates ($cfg.gates | where {|x| not ($x.after_images? | default false) }))
    let after_cfg = ($cfg | update gates ($cfg.gates | where {|x| $x.after_images? | default false }))
    let g = (run_gates $before_cfg [] ($skips | where {|n| $n in ($before_cfg.gates | get name) }))
    if ($g.failed_blocking | is-not-empty) {
        print -e $"  ERROR: blocking gates failed: ($g.failed_blocking | str join ', '). Nothing was changed."
        exit 1
    }

    print ""
    print "== the evidence"
    let c = (current_suggestion)
    print (suggest_report $c.suggestion $c.previous $c.rev)
    let suggested = $c.suggestion.next
    if $suggested == null and ($version | is-empty) {
        print -e "  nothing to release: only docs, tests and housekeeping since the previous release"
        exit 1
    }
    let chosen = if ($version | is-not-empty) { $version } else if $yes { $suggested } else {
        let answer = (input $"next version [($suggested)]: " | str trim)
        if ($answer | is-empty) { $suggested } else { $answer }
    }
    let version_issues = (version_problems $chosen $c.previous.version (existing_tags))
    if ($version_issues | is-not-empty) { for p in $version_issues { print -e $"  ERROR: ($p)" }; exit 1 }
    let notes = if ($highlights | is-not-empty) { $highlights } else if $yes { "" } else { input "highlights (a line, or empty): " | str trim }

    print ""
    print $"== release v($chosen)"
    let steps = (release_steps $chosen)
    let prepare = ($steps | where step == "prepare" | first)
    let release = ($steps | where step == "release" | first)
    let development = ($steps | where step == "development" | first)
    if $dry_run {
        let lines = (release_plan_lines $chosen $skip_images)
        print "  dry run: nothing is built, written or committed. It would:"
        print ($lines | take 3 | str join "\n")
        print ""
        ^nu ($HERE | path join "changelog.nu") preview --version $chosen --highlights $notes
        print ""
        print ($lines | skip 3 | str join "\n")
        return
    }

    # The prepare commit: the images are built from a commit whose VERSION is the release. A run that stopped after it (a failed
    # build) finds it at HEAD and builds from it again.
    if (head_subject) == $prepare.message and ((read_version_text (open --raw (version_file))).version == $chosen) {
        print $"  ($prepare.message) is already HEAD: building from it"
    } else {
        let go = if $yes { true } else { (input $"write ($chosen) into VERSION, commit `($prepare.message)` and build from it? [y/N] " | str trim | str lowercase) in ["y" "yes"] }
        if not $go { print "  stopped: nothing was changed."; return }
        commit_version_step $prepare
    }
    let built_from = (head_sha)
    let undo = $"the prepare commit ($built_from | str substring 0..11) is HEAD: run again to build from it, or drop it with git reset --keep HEAD~1"

    # The images: one coherent build. This machine's lock is moved aside so that only what this build pushes is in it.
    let dev = (lock dev_path)
    let aside = $"($dev).before-release"
    if not $skip_images {
        if ($dev | path exists) { ^mv $dev $aside }
        let built = (do { ^nu ($HERE | path join "images.nu") sync --all } | complete)
        print ($built.stdout | lines | last 12 | str join "\n")
        if $built.exit_code != 0 {
            if ($aside | path exists) { ^mv $aside $dev }
            print -e $"  ERROR: building and pushing the images failed; nothing else was committed: ($undo).\n($built.stderr | lines | last 6 | str join "\n")"
            exit 1
        }
    }
    let problems = (lock lock_problems (lock effective))
    if ($problems | is-not-empty) { for p in $problems { print -e $"  ERROR: ($p)" }; print -e $"  ($undo)"; exit 1 }
    # Images that were not built here were not built from the prepare commit, and the lock does not say they were.
    let lock_from = (if $skip_images { "" } else { $built_from })
    lock promote --built-from $lock_from | ignore
    if ($aside | path exists) { ^rm -f $aside }
    if ($after_cfg.gates | is-not-empty) {
        print ""
        print "== gates that need the lock"
        let after = (run_gates $after_cfg [] ($skips | where {|n| $n in ($after_cfg.gates | get name) }))
        if ($after.failed_blocking | is-not-empty) {
            print -e $"  ERROR: blocking gates failed: ($after.failed_blocking | str join ', '). infra/images.lock.nuon is written and uncommitted; nothing more was committed or tagged: ($undo)."
            exit 1
        }
    }
    ^nu ($HERE | path join "changelog.nu") write --version $chosen --highlights $notes --built-from $lock_from

    let go = if $yes { true } else { (input $"commit and tag ($release.tag)? [y/N] " | str trim | str lowercase) in ["y" "yes"] }
    if not $go { print $"  stopped before committing. infra/images.lock.nuon and CHANGELOG.md are written and uncommitted; ($undo)."; return }
    commit_files $release.files $release.message
    # Building the tag must give the locked digests: the release commit changes nothing an image is built from.
    let changed = (^git -C (repo_root) diff --name-only $built_from HEAD | lines | where {|l| $l | is-not-empty })
    let diff_problems = (release_diff_problems $changed $chosen)
    if ($diff_problems | is-not-empty) {
        for p in $diff_problems { print -e $"  ERROR: ($p)" }
        print -e "  the release commit is made and not tagged: look at it, and drop it with git reset --keep HEAD~1"
        exit 1
    }
    ^git -C (repo_root) tag -a $release.tag -m $"Release ($release.tag)" -m $"Built from ($built_from) \(($prepare.message)\)."
    commit_version_step $development
    print $"  committed ($prepare.message), ($release.message) \(tagged ($release.tag)\) and ($development.message)."
    print $"  Nothing was pushed: git push origin HEAD ($release.tag)"
}

def main [] {
    print "usage: nu infra/release.nu plan | gates | run | secrets"
}
