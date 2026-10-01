#!/usr/bin/env nu
# infra/images.nu
#
# Builds and publishes container images to the local registry, driven by
# infra/images.nuon. Used two ways:
#
#   as a module (infra/up.nu):   use images.nu; images sync (images needed $files)
#   as a script (the Justfile):  nu infra/images.nu push kube-observer jira-observer
#                                nu infra/images.nu push --all
#                                nu infra/images.nu list
#
# Environment (all optional):
#   POLAR_REGISTRY         where this machine pushes     (default localhost:5000)
#   POLAR_CLUSTER_RUNTIME  "u7s" drops the node's cached copy of a rebuilt
#                          image so the next pull is fresh (default u7s;
#                          anything else skips that)
#   POLAR_NEO4J_IMAGE      path to the Neo4j docker-archive (overrides the table)

const TABLE = (path self | path dirname | path join "images.nuon")

def registry [] { $env.POLAR_REGISTRY? | default "localhost:5000" }
def runtime [] { $env.POLAR_CLUSTER_RUNTIME? | default "u7s" }

def load_table [] { open $TABLE }

# "10.0.2.2:5000/kube-observer:latest" -> {ref, name, tag}
# A leading path segment that looks like a host (has a "." or ":") is the
# registry and is dropped; the table holds bare names.
def split_ref [ref: string] {
    let parts = ($ref | split row "/")
    let path = if ($parts | length) > 1 and (($parts | first) =~ '[.:]') {
        $parts | skip 1 | str join "/"
    } else {
        $ref
    }
    let m = ($path | parse --regex '^(?<name>.+?)(?::(?<tag>[^:/]+))?$' | first)
    { ref: $ref, name: $m.name, tag: (if ($m.tag | is-empty) { "latest" } else { $m.tag }) }
}

# Every image referenced by the given manifest files.
def referenced [files: list<string>] {
    $files
    | where {|f| $f | path exists }
    | each {|f|
        open --raw $f
        | parse --regex `(?m)^\s*(?:-\s*)?["']?image["']?\s*:\s*["']?(?<ref>[^"'\s]+)["']?\s*$`
        | each {|r| $r.ref }
    }
    | flatten
    | uniq
    | each {|r| split_ref $r }
}

# The table rows a set of manifests needs. Fails, listing every problem at
# once, when a manifest references an image the table does not know or knows
# under a different tag.
export def needed [files: list<string>] {
    let rows = (load_table)
    mut found = []
    mut problems = []
    for r in (referenced $files) {
        let hit = ($rows | where image == $r.name)
        if ($hit | is-empty) {
            $problems = ($problems | append $"($r.ref): no row for '($r.name)' in infra/images.nuon")
        } else if ($hit | first | get tag) != $r.tag {
            $problems = ($problems | append $"($r.ref): manifests use tag '($r.tag)', infra/images.nuon says '(($hit | first).tag)'")
        } else {
            $found = ($found | append ($hit | first | insert ref $r.ref))
        }
    }
    if ($problems | is-not-empty) {
        let listing = ($problems | str join "\n  ")
        error make { msg: $"image table and manifests disagree:\n  ($listing)" }
    }
    $found | uniq-by image
}

# Table rows by name (for building by hand). No cache invalidation: ref is empty.
export def named [names: list<string>] {
    let rows = (load_table)
    let unknown = ($names | where {|n| $n not-in ($rows | get image) })
    if ($unknown | is-not-empty) {
        let known = ($rows | get image | str join ", ")
        let bad = ($unknown | str join ", ")
        error make { msg: $"unknown image\(s\): ($bad). Known: ($known)" }
    }
    $rows | where image in $names | insert ref ""
}

def dest [row: record] { $"($row.image):($row.tag)" }

def in_registry [row: record] {
    let r = (do { ^skopeo inspect --tls-verify=false --raw $"docker://(registry)/(dest $row)" } | complete)
    $r.exit_code == 0
}

def arch [] {
    match (^uname -m | str trim) {
        "x86_64" => "amd64"
        "aarch64" | "arm64" => "arm64"
        $other => $other
    }
}

# The docker-archive for a row, building it first when it is a nix row.
# Returns null for rows with nothing to push.
def archive [row: record] {
    match $row.kind {
        "nix" => {
            let platform = $"(^uname -m | str trim)-linux"
            mkdir result
            let out = $"result/($row.image)"
            ^nix build -L $".#packages.($platform).($row.attr)" -o $out
            if $env.LAST_EXIT_CODE != 0 { error make { msg: $"nix build failed for ($row.image) \(attr ($row.attr)\)" } }
            $out
        }
        "external" => {
            let p = ($env | get -o $row.env | default $row.path | path expand)
            if ($p | path exists) {
                $p
            } else if (in_registry $row) {
                print $"  ($row.image): archive not found at ($p), but the registry already has it; skipping"
                null
            } else {
                error make { msg: $"($row.image): archive not found at ($p). Build it, or set ($row.env) to its path." }
            }
        }
        _ => null
    }
}

def skopeo_ok [label: string] {
    if $env.LAST_EXIT_CODE != 0 { error make { msg: $"skopeo copy failed for ($label)" } }
}

# Build, mirror and push the given rows to the local registry.
export def sync [rows: list] {
    let total = ($rows | length)
    for it in ($rows | enumerate) {
        let row = $it.item
        print $"  [($it.index + 1)/($total)] ($row.image):($row.tag)  \(($row.kind)\)"
        if $row.kind == "mirror" {
            if (in_registry $row) {
                print "      already in the registry"
            } else {
                ^skopeo copy --override-os linux --override-arch (arch) $"docker://($row.upstream)" $"docker://(registry)/(dest $row)" --dest-tls-verify=false
                skopeo_ok $row.image
            }
        } else {
            let a = (archive $row)
            if $a != null {
                ^skopeo copy $"docker-archive:($a)" $"docker://(registry)/(dest $row)" --dest-tls-verify=false
                skopeo_ok $row.image
                # Nodes keep a pulled :latest and never re-pull it (IfNotPresent),
                # so drop their copy of what was just rebuilt.
                if (runtime) == "u7s" and ($row.ref | is-not-empty) {
                    try { do { ^podman exec u7s ctr -n k8s.io images rm $row.ref } | complete | ignore }
                }
            }
        }
    }
}

# ── Script entry points ─────────────────────────────────────────────────────

def main [] { help main }

# Build and push the named images (or every image in the table with --all).
def "main push" [
    ...names: string
    --all
] {
    let rows = if $all { load_table | insert ref "" } else { named $names }
    sync $rows
    print $"pushed ($rows | length) image\(s\) to (registry)"
}

# List the images in the table.
def "main list" [] {
    load_table | select image tag kind
}
