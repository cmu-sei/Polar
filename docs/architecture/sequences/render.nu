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

# docs/architecture/sequences/render.nu
#
# Renders each Mermaid diagram here to an SVG beside it, with the shared config and theme (assets/): a sequence
# diagram with gruvbox_sequence.css, anything else with gruvbox.css. The SVG opens with the diagram's license
# banner as an XML comment, so the generated file carries it. A bare `%%` line in the banner is sent to Mermaid as
# `%% `: the flowchart parser runs bare ones together and fails, and the source keeps its banner as it is. Flowchart
# labels are SVG text, not HTML (`htmlLabels: false` in assets/mermaid-config.json), so viewers other than browsers
# show them.
#
#   nu docs/architecture/sequences/render.nu                         # every diagram
#   nu docs/architecture/sequences/render.nu general/foo.mmd         # the ones named, relative to this directory

def main [...files: string] {
    cd $env.FILE_PWD
    let files = if ($files | is-empty) { glob "{broker,general,bootstrap}/*.mmd" | sort } else { $files }
    let failed = ($files | each {|file|
        try { render $file; print $"  ($file | path expand | path relative-to $env.FILE_PWD)"; null } catch {|e| print -e $"  FAILED ($file): ($e.msg)"; $file }
    } | compact)
    if ($failed | is-not-empty) { error make --unspanned { msg: $"($failed | length) diagram\(s\) did not render" } }
}

def render [file: string] {
    let source = (open --raw $file)
    let banner = ($source | lines | take while {|l| $l starts-with "%%" } | each {|l| $l | str replace --regex '^%% ?' '' })
    let kind = ($source | lines | where {|l| not ($l starts-with "%%") and ($l | str trim | is-not-empty) } | first | str trim)
    let css = if ($kind starts-with "sequenceDiagram") { "assets/gruvbox_sequence.css" } else { "assets/gruvbox.css" }
    let tmp = (mktemp --suffix .svg)
    let r = ($source | str replace --all --regex '(?m)^%%$' '%% ' | do { ^mmdc -q -i - -o $tmp --configFile assets/mermaid-config.json --cssFile $css --width 2560 --height 1440 } | complete)
    if $r.exit_code != 0 {
        rm -f $tmp
        error make --unspanned { msg: ($r.stderr | lines | where {|l| $l =~ 'Error|error' } | first 2 | str join "; ") }
    }
    # Mermaid puts each word of a label in a tspan of its own, with the space before it inside; viewers other than
    # browsers (librsvg) drop that space unless whitespace is preserved.
    let svg = (open --raw $tmp | str replace '<svg ' '<svg xml:space="preserve" ')
    rm $tmp
    $"<!--\n($banner | str join "\n")\n-->\n($svg)" | save --force --raw ($file | str replace --regex '\.mmd$' '.svg')
}
