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

# infra/cache.nu
#
# A cache for what is a pure function of its inputs and slow to compute: render-catalog.nu keeps the manifest Dhall
# renders from a workload's inputs (about 3.5 seconds each, a minute for the catalog), so a resume that changed
# nothing reads them back. Keyed on a hash of every input, so a change to any one of them is a different key and
# nothing stale can be served. Set POLAR_CACHE=off to compute everything and store nothing.

# Where the cache lives: POLAR_CACHE_DIR, else under XDG_CACHE_HOME, else under HOME.
export def cache_dir [] {
    let explicit = ($env.POLAR_CACHE_DIR? | default "")
    if ($explicit | is-not-empty) { return $explicit }
    let xdg = ($env.XDG_CACHE_HOME? | default "")
    if ($xdg | is-not-empty) { return ($xdg | path join "polar") }
    $env.HOME | path join ".cache" "polar"
}

# A key for a list of inputs. The parts are length-prefixed, so ["ab", ""] and ["a", "b"] cannot give the same key.
export def content_key [parts: list<string>] {
    $parts | each {|p| $"($p | str length):($p)" } | str join "|" | hash sha256
}

# A digest of the files of `dir` matching `pattern`: each name with its content, in name order, so a file edited,
# added, removed or renamed changes it, and a file that does not match does not.
export def dir_digest [dir: string, pattern: string] {
    glob ($dir | path join $pattern)
    | sort
    | each {|f| $"($f | path basename):(open --raw $f | hash sha256)" }
    | content_key $in
}

# Returns the text stored for `key` under `kind`, or computes it with `compute`, stores it and returns it. What
# `compute` returns when it fails is not stored: the error reaches the caller. Written to a file of its own name
# and moved into place, so a reader never sees half of one.
export def cached_text [kind: string, key: string, compute: closure] {
    if ($env.POLAR_CACHE? | default "" | str downcase) == "off" { return (do $compute) }
    let dir = ((cache_dir) | path join $kind)
    let file = ($dir | path join $"($key).txt")
    if ($file | path exists) {
        let hit = (try { open --raw $file } catch { null })
        if $hit != null { return $hit }
    }
    let value = (do $compute)
    mkdir $dir
    let part = $"($file).part-(random chars --length 8)"
    $value | save --force $part
    mv --force $part $file
    $value
}
