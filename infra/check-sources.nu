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

# infra/check-sources.nu
#
# Whether every source the build fetches can still be fetched, anonymously, at the revision its lock file pins. A
# Cargo git dependency that vanishes from GitHub keeps building on a machine that has it cached, and fails for the
# next person with a request for GitHub credentials (serde_dhall's dhall-rust fork did, at 776439e8). This reads
# every Cargo.lock and flake.lock git tracks, checks each (URL, revision) once, and prints one line per failure and
# a summary.
#
#   nu infra/check-sources.nu                          every Cargo.lock and flake.lock in the repository
#   nu infra/check-sources.nu src/agents/Cargo.lock    only these (paths from the repository root)
#   just check-sources
#
# A Cargo git source, a flake `git` input and a `gitlab` one are checked by `git fetch --depth 1 URL REV` into a
# scratch repository; a `github` input by its archive URL (github.com/OWNER/REPO/archive/REV.tar.gz, what Nix
# downloads), redirects followed, since GitHub answers 302 even for a revision it does not have and the 404 comes
# from codeload; a `tarball` or remote `file` input by its locked URL. A 301 on the way means the repository moved:
# Nix follows it, so it passes, and is said. `path` inputs and local files and repositories are not fetched and not
# checked. A type this script does not know, an ssh URL, or a git source with no revision fails.
#
# It never uses the caller's credentials: git runs with no global or system config (no credential helper, no
# insteadOf rewriting https to ssh), no askpass and no terminal prompt, so a repository that has gone answers
# "terminal prompts disabled" instead of asking; curl runs with -q (no .curlrc) and without --netrc.
#
# Exit 0 when every source is fetchable, 1 when any is not, 2 (EXIT_OFFLINE) when github.com cannot be reached at
# all, in which case nothing is checked: being offline is not a missing source.

const REPO = (path self | path dirname | path dirname)

export const EXIT_OFFLINE = 2

# Per check. git's limit is on `timeout`, curl's on itself.
const GIT_SECS = 60
const CURL_SECS = 30
const THREADS = 8

# How a git URL is checked: fetched when anonymous access is possible, refused when it needs credentials, not at all
# when it is local (null).
def git_check [url: string, rev: string, what: string, from: string] {
    if ($url =~ '^(https?|git)://') {
        { check: "git", url: $url, rev: $rev, what: $what, from: $from }
    } else if ($url =~ '^(file:|path:|/|\.)') {
        null
    } else {
        { check: "unsupported", url: $url, rev: $rev, what: $what, from: $from, detail: "not an https or git:// URL: fetching it needs credentials" }
    }
}

# A Cargo.lock `source` value as { url, rev }: the URL without `git+` and its ?branch=/?rev=/?tag= query, the
# revision after `#` ("" when there is none). null for a registry or any other non-git source.
export def cargo_source [source: string] {
    if not ($source | str starts-with "git+") { return null }
    let parts = ($source | str substring 4.. | split row "#")
    { url: ($parts | first | split row "?" | first), rev: ($parts | get -o 1 | default "") }
}

# The git sources in a Cargo.lock's text, one per package.
export def cargo_sources [text: string, from: string] {
    $text | from toml | get -o package | default [] | each {|p|
        let s = (cargo_source ($p.source? | default ""))
        if $s == null { null } else { git_check $s.url $s.rev $"crate ($p.name)" $from }
    } | compact
}

# One flake.lock node's locked reference as a check, or null when it is not fetched from the network.
def flake_node [name: string, l: record, from: string] {
    let what = $"flake input ($name)"
    match $l.type {
        "github" => {
            let host = ($l.host? | default "github.com")
            { check: "http", url: $"https://($host)/($l.owner)/($l.repo)/archive/($l.rev).tar.gz", rev: $l.rev, what: $what, from: $from }
        }
        "gitlab" => {
            let host = ($l.host? | default "gitlab.com")
            git_check $"https://($host)/($l.owner | url decode)/($l.repo).git" ($l.rev? | default "") $what $from
        }
        "git" => { git_check $l.url ($l.rev? | default "") $what $from }
        "tarball" | "file" => {
            if ($l.url =~ '^https?://') { { check: "http", url: $l.url, rev: "", what: $what, from: $from } } else { null }
        }
        "path" => null
        _ => {
            let id = ([$l.owner? $l.repo? $l.url?] | compact | str join "/")
            {
                check: "unsupported", url: $"($l.type):($id)", rev: ($l.rev? | default ""), what: $what, from: $from
                detail: $"flake input type ($l.type) is not checked by this script"
            }
        }
    }
}

# The remote inputs in a flake.lock's text.
export def flake_sources [text: string, from: string] {
    $text | from json | get nodes | transpose name node
    | where {|r| $r.node.locked? != null }
    | each {|r| flake_node $r.name $r.node.locked $from }
    | compact
}

# One check per (check, url, rev), saying everything that uses it; sorted, so independent of reading order.
export def unique_checks [sources: list] {
    $sources
    | group-by {|s| $"($s.check) ($s.url) ($s.rev)" }
    | values
    | each {|g|
        let s = ($g | first)
        {
            check: $s.check
            url: $s.url
            rev: $s.rev
            used_by: ($g | each {|x| $"($x.from): ($x.what)" } | uniq | sort)
            detail: ($s.detail? | default "")
        }
    }
    | sort-by url rev check
}

# One HTTP status: 200 and 302 ok, 301 moved (Nix follows it), 404 missing, anything else an error.
export def classify_http [code: int] {
    match $code {
        200 | 302 => "ok"
        301 => "moved"
        404 => "missing"
        _ => "error"
    }
}

# A redirect chain, judged by where it ends; ok by way of a 301 is moved.
export def http_verdict [codes: list<int>] {
    if ($codes | is-empty) { return "error" }
    let last = (classify_http ($codes | last))
    if $last == "ok" and (301 in $codes) { "moved" } else { $last }
}

# What git's answer to the fetch means. Asking for credentials is how GitHub answers for a repository that is gone,
# renamed without a redirect, or private.
export def git_verdict [exit_code: int, stderr: string] {
    if $exit_code == 0 { return "ok" }
    if $exit_code == 124 { return "timeout" }
    let missing = ["terminal prompts disabled" "could not read Username" "Repository not found" "not our ref" "couldn't find remote ref"]
    if ($missing | any {|m| $stderr | str contains $m }) { "missing" } else { "error" }
}

# The environment git runs in: none of the caller's configuration, and no way to ask for a password.
export def git_env [] {
    {
        GIT_TERMINAL_PROMPT: "0"
        GIT_ASKPASS: ""
        SSH_ASKPASS: ""
        GIT_CONFIG_GLOBAL: "/dev/null"
        GIT_CONFIG_NOSYSTEM: "1"
        GIT_CONFIG_COUNT: "0"
        GIT_CONFIG_PARAMETERS: ""
    }
}

export def git_fetch_args [dir: string, url: string, rev: string] {
    [-C $dir -c credential.helper= -c core.askPass= fetch --quiet --depth "1" --filter=tree:0 --no-tags $url $rev]
}

# -q first: no .curlrc. No --netrc. The headers of every hop to stdout, the body nowhere.
export def curl_args [url: string] {
    [-q -sS -I -L --max-redirs "5" --connect-timeout "10" --max-time ($CURL_SECS | into string) -o /dev/null -D - $url]
}

def users [used_by: list] {
    let n = ($used_by | length)
    if $n > 3 { ($used_by | first 3 | str join ", ") + $" and ($n - 3) more" } else { $used_by | str join ", " }
}

def line_for [r: record] {
    let at = if $r.rev == "" { $r.url } else { $"($r.url) @ ($r.rev)" }
    $"($at) \(used by (users $r.used_by)\)" + (if $r.detail == "" { "" } else { $": ($r.detail)" })
}

# Runs every check (in parallel) and says what it found: { lines, exit_code }. `reachable` says whether github.com
# answers at all; `run` checks one source, returning { verdict, detail }. Unsupported and unpinned sources fail
# without being run.
export def check_all [checks: list, reachable: closure, run: closure] {
    if not (do $reachable) {
        return {
            lines: [$"github.com cannot be reached, so none of the ($checks | length) sources was checked. Offline? Run this again with a network."]
            exit_code: $EXIT_OFFLINE
        }
    }
    let results = ($checks | par-each --threads $THREADS {|c|
        let c = ($c | upsert detail ($c.detail? | default ""))
        if $c.check == "unsupported" {
            $c | merge { verdict: "unsupported" }
        } else if $c.check == "git" and $c.rev == "" {
            $c | merge { verdict: "unpinned", detail: "the lock pins no revision" }
        } else {
            $c | merge (do $run $c)
        }
    } | sort-by url rev)
    let failed = ($results | where verdict not-in ["ok" "moved"])
    let moved = ($results | where verdict == "moved")
    let ok = ($results | length) - ($failed | length) - ($moved | length)
    let lines = (
        ($failed | each {|r| $"FAIL  ($r.verdict)  (line_for $r)" })
        | append ($moved | each {|r| $"moved ($r.url) \(used by (users $r.used_by)\): Nix follows the redirect, but the lock should name the new place" })
        | append $"($results | length) sources checked: ($ok) ok, ($moved | length) moved, ($failed | length) failed"
    )
    { lines: $lines, exit_code: (if ($failed | is-empty) { 0 } else { 1 }) }
}

def github_reachable [] {
    (^curl -q -sS -o /dev/null --connect-timeout 10 --max-time 15 https://github.com/ | complete).exit_code == 0
}

def first_line [s: string] { $s | lines | each { str trim } | where {|l| $l | is-not-empty } | get -o 0 | default "" }

# Checks one source against the network.
def run_check [c: record] {
    if $c.check == "http" {
        let r = (^curl ...(curl_args $c.url) | complete)
        let codes = ($r.stdout | lines | parse --regex '^HTTP/\S+ (?<code>\d{3})' | get code | each { into int })
        let detail = if ($codes | is-empty) { first_line $r.stderr } else { $"HTTP ($codes | str join ' -> ')" }
        { verdict: (http_verdict $codes), detail: $detail }
    } else {
        let dir = (mktemp -d -t polar-check-sources.XXXXXX)
        let r = (with-env (git_env) {
            ^git init --quiet $dir | complete
            ^timeout $GIT_SECS git ...(git_fetch_args $dir $c.url $c.rev) | complete
        })
        rm -rf $dir
        let verdict = (git_verdict $r.exit_code $r.stderr)
        let detail = match $verdict {
            "ok" => ""
            "timeout" => $"no answer in ($GIT_SECS)s"
            _ => (if ($r.stderr | str contains "terminal prompts disabled") {
                "the server asks for credentials: the repository is gone, renamed or private"
            } else { first_line $r.stderr })
        }
        { verdict: $verdict, detail: $detail }
    }
}

def sources_in [file: string] {
    let text = (open --raw ($REPO | path join $file))
    match ($file | path basename) {
        "Cargo.lock" => (cargo_sources $text $file)
        "flake.lock" => (flake_sources $text $file)
        _ => (error make { msg: $"($file): not a Cargo.lock or a flake.lock" })
    }
}

def main [...locks: string] {
    let files = if ($locks | is-empty) {
        ^git -C $REPO ls-files -- '*Cargo.lock' '*flake.lock' | lines | where {|l| $l | is-not-empty }
    } else { $locks }
    let checks = (unique_checks ($files | each {|f| sources_in $f } | flatten))
    print $"checking ($checks | length) sources from ($files | str join ', ')"
    let r = (check_all $checks { github_reachable } {|c| run_check $c })
    for l in $r.lines { if ($l | str starts-with "FAIL") or $r.exit_code == $EXIT_OFFLINE { print -e $l } else { print $l } }
    exit $r.exit_code
}
