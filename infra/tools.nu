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

# infra/tools.nu
#
# The standalone Polar tools (polar-sign, polar-admin-grant, cassini-c2, cassini-client,
# user-cert-fetcher, cert-client, cert-issuer-setup) are built as static binaries and shipped in
# the `polar-tools` image. Nothing has to be compiled to use them: copy them out of the image
# into a directory of your choosing and put that directory on your PATH.
#
#   nu infra/tools.nu install ~/.local/bin                  from the registry (POLAR_REGISTRY)
#   nu infra/tools.nu install ~/.local/bin --from docker://registry.example.org/polar-tools:latest
#   nu infra/tools.nu install ~/.local/bin --from docker-archive:result/polar-tools
#
# `--from` is any skopeo reference to the image. Needs `skopeo` and `tar`. No container runtime.
#
# The scripts that use the tools (up.nu, cassini-shutdown.nu, the Justfile's admin recipes) call
# `command`, below: the tool on PATH when there is one, and otherwise `cargo run` from this checkout,
# which is what a developer with a Rust toolchain wants.

# Where the tools sit inside the image.
const IMAGE_DIR = "opt/polar/bin"

# This directory, so the cargo fallback finds the workspace from wherever it is called.
const HERE = (path self | path dirname)

def registry [] { $env.POLAR_REGISTRY? | default "localhost:5000" }

# A local registry speaks plain HTTP.
def registry_flags [] {
    let r = (registry)
    if ($r | str starts-with "localhost") or ($r | str starts-with "127.") { ["--src-tls-verify=false"] } else { [] }
}

# The command that runs tool `name`, as a list to spread into an external call:
#   let t = (tools command "polar-sign"); ^($t | first) ...($t | skip 1) sign ...
export def command [name: string] {
    if (which $name | is-not-empty) {
        [$name]
    } else {
        ["cargo" "run" "-q" "--manifest-path" ($HERE | path join "../src/agents/Cargo.toml" | path expand) "-p" $name "--"]
    }
}

# Copies the tools out of the polar-tools image into `dir` (created if it is not there).
def "main install" [
    dir: string                 # where to put the binaries; put this directory on your PATH
    --from: string = ""         # a skopeo reference to the image (default: the registry's polar-tools:latest)
] {
    let source = (if ($from | is-empty) { $"docker://(registry)/polar-tools:latest" } else { $from })
    let flags = (if ($from | is-empty) { registry_flags } else { [] })
    let work = (^mktemp -d | str trim)
    let image = ($work | path join "image")
    let files = ($work | path join "files")
    mkdir $files

    # skopeo stages in /var/tmp unless told: it does not read TMPDIR.
    let tmp = (if ($env.TMPDIR? | default "" | is-empty) { [] } else { ["--tmpdir" $env.TMPDIR] })
    let copied = (do { ^skopeo ...$tmp copy ...$flags $source $"dir:($image)" } | complete)
    if $copied.exit_code != 0 {
        rm -rf $work
        error make --unspanned { msg: $"skopeo could not read ($source): ($copied.stderr | str trim)" }
    }

    # `dir:` writes manifest.json and one file per layer, named by the layer's digest.
    let manifest = (open ($image | path join "manifest.json"))
    for layer in $manifest.layers {
        let blob = ($image | path join ($layer.digest | str replace "sha256:" ""))
        # A layer without the tools is not an error: tar says so and we move on.
        # (The layer's paths start with "./"; the pattern accepts that and the bare form.)
        do { ^tar -xf $blob -C $files --wildcards $"*($IMAGE_DIR)/*" } | complete | ignore
    }

    let found = (try { ls ($files | path join $IMAGE_DIR) | get name } catch { [] })
    if ($found | is-empty) {
        rm -rf $work
        error make --unspanned { msg: $"no tools in ($source): expected files under /($IMAGE_DIR) in the image" }
    }

    let target = ($dir | path expand)
    mkdir $target
    for f in $found {
        let dest = ($target | path join ($f | path basename))
        cp $f $dest
        ^chmod 755 $dest
    }
    rm -rf $work
    print $"Installed to ($target):"
    for f in $found { print $"  ($f | path basename)" }
    if not ($env.PATH | any {|p| ($p | path expand) == $target }) {
        print $"\nThat directory is not on your PATH yet. In fish: fish_add_path ($target)   In bash: export PATH=\"($target):$PATH\""
    }
    print "polar-sign also needs `git` on your PATH."
}

def main [] {
    print "usage: nu infra/tools.nu install <dir> [--from <skopeo reference>]"
}
