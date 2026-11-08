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

# infra/dhall-render.nu
#
# `dhall-to-yaml` for the hand-written charts, kept in the render cache (infra/cache.nu). A render of a Dhall file is a function of
# the text rendered, of every local Dhall file it can import (a chart's own, the shared schema, the target's overrides: they are
# imported by path, so none of them is in the text), of the tool's version and of the flags; each of those is in the key, so a
# change to any of them is a different key and nothing stale is served. About two seconds a file is saved on a repeat render.
#
# Not in the key: the content of a Dhall import by URL (the Prelude). The catalog render's cache leaves it out too.

use cache.nu [cached_text, content_key, dir_digest]

const INFRA = (path self | path dirname)

# The inputs a render of `file` with `flags` depends on, as the parts of its cache key.
export def key_parts [text: string, sources: string, version: string, flags: list<string>] {
    ["chart" $text $sources $version ($flags | str join " ")]
}

# The names of the environment variables the Dhall texts import (`env:NAME`), sorted and once each. A render depends on their values.
export def env_names_in [texts: list<string>] {
    $texts | each {|t| $t | parse --regex 'env:(?<name>[A-Za-z_][A-Za-z0-9_]*)' | get name } | flatten | uniq | sort
}

# The sources part of the key: every local Dhall file, and the value of every environment variable one of them imports (the registry, for
# one: render.nu hands it to the charts that way), so a changed value is another key.
def sources_key [] {
    let files = (glob ($INFRA | path join "**" "*.dhall") | sort)
    let names = (env_names_in ($files | each {|f| open --raw $f }))
    let values = ($names | each {|n| $"($n)=($env | get -o $n | default '')" } | str join "\n")
    $"(dir_digest $INFRA '**/*.dhall')|($values)"
}

# `dhall-to-yaml <flags> --file <file>`, as text; the same call again with nothing changed reads it back.
export def dhall_yaml [file: string, flags: list<string> = []] {
    let parts = (key_parts (open --raw $file) (sources_key) (^dhall-to-yaml --version | str trim) $flags)
    cached_text "chart" (content_key $parts) {||
        let r = (^dhall-to-yaml ...$flags --file $file | complete)
        if $r.exit_code != 0 { error make { msg: $"dhall-to-yaml failed on ($file): ($r.stderr | str trim)" } }
        $r.stdout
    }
}
