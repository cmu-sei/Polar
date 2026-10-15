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

# infra/measure-render-cache.nu
#
# Measures what the render cache saves and writes a markdown table (docs: infra/INFRA.md, "The render cache").
# Run from the repo root, with KUBECONFIG set if the up.nu cases are wanted:
#
#   nu infra/measure-render-cache.nu results.md                      # the render cases
#   nu infra/measure-render-cache.nu results.md --user U --email E   # and a no-op up.nu resume (needs a cluster)
#
# "From a clean state" means a fresh, empty cache directory for each cold run, and POLAR_CACHE=off for the
# uncached ones. Dhall's own import cache (~/.cache/dhall) is left as it is, and so are the page caches.
# Takes about 16 minutes on a machine with 32 logical CPUs; leave it alone while it runs, or the timings move.

def secs [d: duration] { ($d / 1sec) }
def fmt [x: float] { $x | math round --precision 1 }

# Runs `body` n times and returns the seconds of each run.
def runs [n: int, body: closure] {
    1..$n | each {|i| secs (timeit { do $body $i | ignore }) }
}

def row [label: string, times: list<float>] {
    {
        case: $label
        runs: ($times | length)
        mean_s: (fmt ($times | math avg))
        min_s: (fmt ($times | math min))
        max_s: (fmt ($times | math max))
    }
}

def main [results: string, --user: string = "", --email: string = ""] {
    let root = $env.PWD
    let infra = ($root | path join "infra")
    let agent_source = { POLAR_AGENT_SOURCE: "images" }
    let n = 3
    mut rows = []

    # ── A. the catalog render (what the cache changes) ─────────────────────
    let out = (mktemp --directory)
    print "A1 catalog, cache off"
    $rows = ($rows | append (row "catalog render, cache off" (runs $n {|i|
        with-env ($agent_source | merge { POLAR_CACHE: "off" }) { ^nu ($infra | path join "render-catalog.nu") local-minimal $out } })))

    print "A2 catalog, cold cache (a fresh empty cache directory each run)"
    $rows = ($rows | append (row "catalog render, cold cache" (runs $n {|i|
        let cd = (mktemp --directory)
        let t = (with-env ($agent_source | merge { POLAR_CACHE_DIR: $cd }) { ^nu ($infra | path join "render-catalog.nu") local-minimal $out })
        rm -rf $cd
        $t } )))

    print "A3 catalog, warm cache"
    let warm_dir = (mktemp --directory)
    with-env ($agent_source | merge { POLAR_CACHE_DIR: $warm_dir }) { ^nu ($infra | path join "render-catalog.nu") local-minimal $out | ignore }
    let entries = (ls ($warm_dir | path join "render") | length)
    let bytes = (ls ($warm_dir | path join "render") | get size | math sum)
    $rows = ($rows | append (row "catalog render, warm cache" (runs $n {|i|
        with-env ($agent_source | merge { POLAR_CACHE_DIR: $warm_dir }) { ^nu ($infra | path join "render-catalog.nu") local-minimal $out } })))

    print "A4 catalog, warm cache, one workload's input changed"
    mkdir ($infra | path join "targets/zz-measure")
    let cfg = (open --raw ($infra | path join "targets/local-minimal/config.nuon"))
    $rows = ($rows | append (row "catalog render, warm cache, one workload changed" (runs $n {|i|
        # a different pull policy each time, so it is never in the cache already
        let policy = (if ($i mod 2) == 0 { "Never" } else { "Always" })
        let changed = ($cfg | str replace '    kube-observer: { issuer: "credentials-agent" }' $'    kube-observer: { issuer: "credentials-agent", pull_policy: "($policy)", shutdown: { budget_secs: (40 + $i), grace_margin_secs: 10 } }')
        $changed | save --force ($infra | path join "targets/zz-measure/config.nuon")
        with-env ($agent_source | merge { POLAR_CACHE_DIR: $warm_dir }) { ^nu ($infra | path join "render-catalog.nu") zz-measure $out } })))
    rm -rf ($infra | path join "targets/zz-measure")

    # ── B. the whole render that up.nu runs ───────────────────────────────
    print "B1 whole render.nu, cache off"
    $rows = ($rows | append (row "whole render (render.nu), cache off" (runs 2 {|i|
        with-env ($agent_source | merge { POLAR_CACHE: "off" }) { ^nu ($infra | path join "render.nu") local-minimal } })))
    print "B2 whole render.nu, warm cache"
    with-env ($agent_source | merge { POLAR_CACHE_DIR: $warm_dir }) { ^nu ($infra | path join "render.nu") local-minimal | ignore }
    $rows = ($rows | append (row "whole render (render.nu), warm cache" (runs $n {|i|
        with-env ($agent_source | merge { POLAR_CACHE_DIR: $warm_dir }) { ^nu ($infra | path join "render.nu") local-minimal } })))

    # ── C. a no-op resume of up.nu, end to end ─────────────────────────────
    if ($user | is-not-empty) and ($email | is-not-empty) {
        let up = {|env_extra|
            with-env $env_extra { ^nu ($infra | path join "up.nu") local-minimal --user $user --email $email }
        }
        print "C1 up.nu resume, cache off"
        $rows = ($rows | append (row "up.nu resume that changes nothing, cache off" (runs 2 {|i| do $up { POLAR_CACHE: "off" } })))
        print "C2 up.nu resume, warm cache"
        do $up { POLAR_CACHE_DIR: $warm_dir } | ignore
        $rows = ($rows | append (row "up.nu resume that changes nothing, warm cache" (runs $n {|i| do $up { POLAR_CACHE_DIR: $warm_dir } })))
    }

    rm -rf $out $warm_dir
    let table = ($rows | to md --pretty)
    let cpus = (sys cpu | length)
    let text = $"($table)\n\ncache: ($entries) entries, ($bytes) on disk for the catalog; ($cpus) logical CPUs\n"
    $text | save --force $results
    print $text
}
