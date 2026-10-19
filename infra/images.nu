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

# infra/images.nu
#
# Builds and publishes container images to the local registry, driven by
# infra/images.nuon. Used two ways:
#
#   as a module (infra/up.nu):   use images.nu; images sync (images needed $files)
#   as a script (the Justfile):  nu infra/images.nu build --all
#                                nu infra/images.nu push kube-observer jira-observer
#                                nu infra/images.nu sync --all
#                                nu infra/images.nu list
#
# Stages. Each needs only what is listed, so the earlier ones work on a machine
# with no registry and no cluster:
#
#   build   builds the nix images (leaves archives under result/)
#           needs: nix
#   push    build, then pushes nix images and the external archives to the registry
#           needs: nix, the registry. Never touches the cluster
#   mirror  copies third-party images from upstream into the registry
#           needs: the registry, the internet
#   sync    every stage above, then drops the cluster node's stale copy of what
#           was rebuilt (this is what up.nu and `just image` use)
#           needs: all of the above; the node cache step is skipped when no node exists
#
# A failing image is reported and the stage carries on with the rest; the stage
# ends with a summary and a non-zero exit when anything failed.
#
# Environment (all optional):
#   POLAR_REGISTRY         where this machine pushes     (default localhost:5000)
#   POLAR_CLUSTER_RUNTIME  "u7s" drops the node's cached copy of a rebuilt
#                          image so the next pull is fresh (default u7s;
#                          anything else skips that)
#   POLAR_NEO4J_IMAGE      path to the Neo4j docker-archive (overrides the table)

use lock.nu
use registry.nu [node_registry]

const TABLE = (path self | path dirname | path join "images.nuon")

def registry [] { $env.POLAR_REGISTRY? | default "localhost:5000" }
def runtime [] { $env.POLAR_CLUSTER_RUNTIME? | default "u7s" }

# The registry address as the cluster nodes see it, which is not the address this
# machine pushes to. A node's cached image is named with this one.

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
        error make --unspanned { msg: $"image table and manifests disagree:\n  ($listing)" }
    }
    $found | uniq-by image
}

# Table rows by name (for building by hand). ref is empty; a node's cached copy is
# evicted by name and tag, not through ref.
export def named [names: list<string>] {
    let rows = (load_table)
    let unknown = ($names | where {|n| $n not-in ($rows | get image) })
    if ($unknown | is-not-empty) {
        let known = ($rows | get image | str join ", ")
        let bad = ($unknown | str join ", ")
        error make --unspanned { msg: $"unknown image\(s\): ($bad). Known: ($known)" }
    }
    $rows | where image in $names | insert ref ""
}

def dest [row: record] { $"($row.image):($row.tag)" }

def in_registry [row: record] {
    let r = (do { ^skopeo inspect --tls-verify=false --raw $"docker://(registry)/(dest $row)" } | complete)
    $r.exit_code == 0
}

# The architecture name of a machine type (`uname -m`), as images are tagged and published.
export def arch_of [machine: string] {
    match $machine {
        "x86_64" | "amd64" => "amd64"
        "aarch64" | "arm64" => "arm64"
        $other => (error make --unspanned { msg: $"no images are published for the architecture '($other)'; use amd64 or arm64" })
    }
}

# The Nix system that builds images for an architecture name.
export def system_of [arch: string] {
    match (arch_of $arch) {
        "amd64" => "x86_64-linux"
        _ => "aarch64-linux"
    }
}

# Where a published image lives: `<from>/polar-<image>:<tag>-<arch>`. Public registries such as Docker Hub give an account one
# level of names, so the image name carries the `polar-` prefix and the architecture is in the tag. `import` and `publish` agree on this.
export def published_ref [from: string, row: record, arch: string] {
    $"($from | str trim --right --char '/')/polar-($row.image):($row.tag)-(arch_of $arch)"
}

# The rows `import` and `publish` handle: the images this repository builds (not mirrors of third-party images, and not the
# Neo4j archive, which is built elsewhere), all of them or only those named.
export def published_rows [table: list, names: list<string>] {
    let built = ($table | where kind == "nix")
    if ($names | is-empty) { return $built }
    # A mirror, a third-party image copied from its own registry, is published only when named: a user's `import` takes
    # mirrors from their upstream registries, so a copy on the hub is for those who cannot reach those.
    let nameable = ($table | where kind in ["nix" "mirror"])
    let unknown = ($names | where {|n| $n not-in ($nameable | get image) })
    if ($unknown | is-not-empty) {
        error make --unspanned { msg: $"not an image this repository builds or mirrors: ($unknown | str join ', ')" }
    }
    $nameable | where {|r| $r.image in $names }
}

def arch [] { arch_of (^uname -m | str trim) }

# The docker-archive for a row, building it first when it is a nix row.
# Returns null for rows with nothing to push.
def archive [row: record, system: string = ""] {
    match $row.kind {
        "nix" => {
            let platform = if ($system | is-empty) { $"(^uname -m | str trim)-linux" } else { $system }
            let dir = if ($system | is-empty) { "result" } else { $"result/($system)" }
            mkdir $dir
            let out = $"($dir)/($row.image)"
            try {
                ^nix build -L $".#packages.($platform).($row.attr)" -o $out
            } catch {
                error make --unspanned { msg: $"nix build failed \(attr ($row.attr)\)" }
            }
            $out
        }
        "external" => {
            let p = ($env | get -o $row.env | default $row.path | path expand)
            if ($p | path exists) {
                $p
            } else if (in_registry $row) {
                print $"      archive not found at ($p), but the registry already has it; skipping"
                null
            } else {
                error make --unspanned { msg: $"archive not found at ($p). Build it, or set ($row.env) to its path." }
            }
        }
        _ => null
    }
}

# ── Steps ───────────────────────────────────────────────────────────────────

# The digest the registry holds for a row, written to this machine's image lock (infra/lock.nu). The digest is
# how the image was pushed, so it is read from the registry now and not computed from the archive.
def record_digest [row: record] {
    let r = (do { ^skopeo inspect --tls-verify=false --format '{{.Digest}}' $"docker://(registry)/(dest $row)" } | complete)
    if $r.exit_code != 0 {
        error make --unspanned { msg: $"cannot read the digest of (dest $row) from (registry): ($r.stderr | str trim)" }
    }
    lock record $row.image $row.tag (arch) ($r.stdout | str trim)
}

# The format is named, not left to skopeo's default: the digest of the manifest depends on it.
def push_archive [row: record, a: string] {
    try {
        ^skopeo copy --format v2s2 $"docker-archive:($a)" $"docker://(registry)/(dest $row)" --dest-tls-verify=false
    } catch {
        error make --unspanned { msg: $"skopeo could not push the archive to (registry)" }
    }
    record_digest $row
}

# Upstream registries fail transiently (a 503 on a blob is common), hence the retries.
def mirror_row [row: record] {
    if (in_registry $row) {
        print "      already in the registry"
        record_digest $row
    } else {
        try {
            ^skopeo copy --retry-times 3 --override-os linux --override-arch (arch) $"docker://($row.upstream)" $"docker://(registry)/(dest $row)" --dest-tls-verify=false
        } catch {
            error make --unspanned { msg: $"skopeo could not copy ($row.upstream) into (registry)" }
        }
        record_digest $row
    }
}

# Nodes keep a pulled :latest and never re-pull it (IfNotPresent), so drop their
# copy of what was just rebuilt. The name is the one the node knows the image by,
# built from the row, so every path (by name, --all, from manifests) evicts. A
# failure is reported: a stale cache here looks exactly like running the new code.
def evict_row [row: record] {
    let node_ref = $"(node_registry)/(dest $row)"
    try {
        let r = (do { ^podman exec u7s ctr -n k8s.io images rm $node_ref } | complete)
        if $r.exit_code != 0 and not ($r.stderr | str contains "not found") {
            print $"      could not evict ($node_ref) from the node: ($r.stderr | str trim)"
        }
    } catch {|e|
        print $"      could not evict ($node_ref) from the node: ($e.msg)"
    }
}

# ── Stages ──────────────────────────────────────────────────────────────────

# Which kinds of row each stage handles.
def applies [stage: string, kind: string] {
    match $stage {
        "build" => ($kind == "nix")
        "push" => ($kind in ["nix" "external"])
        "mirror" => ($kind == "mirror")
        "sync" => true
        _ => false
    }
}

# Is there a u7s node whose image cache could be stale?
def node_present [] {
    try { (do { ^podman container exists u7s } | complete).exit_code == 0 } catch { false }
}

def require_registry [] {
    let r = (try { do { ^curl -sf --max-time 3 $"http://(registry)/v2/" } | complete } catch { { exit_code: 1 } })
    if $r.exit_code != 0 {
        error make --unspanned { msg: $"the registry at (registry) is not reachable. Start it with `just registry-up`, or set POLAR_REGISTRY." }
    }
}

def do_row [stage: string, row: record, evict: bool] {
    if $row.kind == "mirror" {
        mirror_row $row
    } else {
        let a = (archive $row)
        if $a != null {
            if $stage == "build" {
                print $"      built: ($a)"
            } else {
                push_archive $row $a
                if $stage == "sync" and $evict { evict_row $row }
            }
        } else if $stage != "build" and (in_registry $row) {
            # Nothing to push because the registry already has it (an external archive): its digest is still the lock's.
            record_digest $row
        }
    }
}

# Runs one stage over the rows it applies to. Returns how many it handled; raises
# once at the end, after trying every row, when any of them failed.
def run_stage [stage: string, rows: list] {
    if $stage in ["push" "mirror" "sync"] { require_registry }

    let todo = ($rows | where {|r| applies $stage $r.kind })
    let skipped = ($rows | where {|r| not (applies $stage $r.kind) } | get image)
    if ($skipped | is-not-empty) {
        let names = ($skipped | str join ", ")
        print $"  not part of the '($stage)' stage: ($names)"
    }

    let evict = ($stage == "sync") and ((runtime) == "u7s") and (node_present)
    if $stage == "sync" and (runtime) == "u7s" and not $evict {
        print "  no u7s node found; leaving node image caches alone"
    }

    let total = ($todo | length)
    mut failures: list<any> = []
    for it in ($todo | enumerate) {
        let row = $it.item
        print $"  [($it.index + 1)/($total)] ($row.image):($row.tag)  \(($row.kind)\)"
        let err = (try { do_row $stage $row $evict; null } catch {|e| $e.msg })
        if $err != null {
            print $"      FAILED: ($err)"
            $failures = ($failures | append { image: $row.image, error: $err })
        }
    }

    if ($failures | is-not-empty) {
        let failed = ($failures | length)
        print ""
        print $"($failed) of ($total) failed in the '($stage)' stage:"
        for f in $failures { print $"  ($f.image): ($f.error)" }
        error make --unspanned { msg: $"($failed) image\(s\) failed in the '($stage)' stage" }
    }
    $total
}

# Every stage, for the given rows. Used by infra/up.nu.
export def sync [rows: list] {
    run_stage "sync" $rows | ignore
}

# ── Script entry points ─────────────────────────────────────────────────────

def main [] { help main }

def rows_for [names: list<string>, all: bool] {
    if $all {
        load_table | insert ref ""
    } else if ($names | is-empty) {
        error make --unspanned { msg: "name at least one image, or pass --all" }
    } else {
        named $names
    }
}

# Stage 1: build the nix images. Needs no registry and no cluster.
def "main build" [
    ...names: string
    --all
    --arch: string = ""   # build for another architecture (amd64 or arm64); the archives go under result/<system>/
] {
    if ($arch | is-not-empty) {
        let system = (system_of $arch)
        let rows = ((rows_for $names $all) | where kind == "nix")
        for row in $rows { print $"  ($row.image) for ($system)"; print $"      built: (archive $row $system)" }
        print $"built ($rows | length) image\(s\) for ($system); the archives are under result/($system)/"
        return
    }
    let n = (run_stage "build" (rows_for $names $all))
    print $"built ($n) image\(s\); the archives are under result/"
}

# Stage 2: build, then push to the registry. Needs the registry, not the cluster.
def "main push" [
    ...names: string
    --all
] {
    let n = (run_stage "push" (rows_for $names $all))
    print $"pushed ($n) image\(s\) to (registry)"
}

# Stage 3: copy third-party images from upstream into the registry. Needs the internet.
def "main mirror" [
    ...names: string
    --all
] {
    let n = (run_stage "mirror" (rows_for $names $all))
    print $"mirrored ($n) image\(s\) into (registry)"
}

# Every stage, then drop the cluster node's stale copies.
def "main sync" [
    ...names: string
    --all
] {
    let n = (run_stage "sync" (rows_for $names $all))
    print $"synced ($n) image\(s\) to (registry)"
}

# The image lock (infra/lock.nu). `refresh` records the digest of every image the registry already holds, without
# building or pushing anything; `promote` makes this machine's lock the committed one, for a release; `show` prints
# the lock a render would use.
def "main lock refresh" [
    ...names: string        # only these images (default: every row)
] {
    require_registry
    let rows = (if ($names | is-empty) { load_table } else { named $names })
    mut recorded = 0
    mut absent = []
    for row in $rows {
        if (in_registry $row) {
            record_digest $row
            $recorded = $recorded + 1
        } else {
            $absent = ($absent | append $"($row.image):($row.tag)")
        }
    }
    print $"recorded the digest of ($recorded) image\(s\) in (lock dev_path)"
    if ($absent | is-not-empty) { print $"not in the registry, so not recorded: ($absent | str join ', ')" }
}

def "main lock promote" [] {
    let problems = (lock lock_problems (lock effective))
    if ($problems | is-not-empty) {
        for p in $problems { print -e $"  ERROR: ($p)" }
        error make --unspanned { msg: "the lock has problems; nothing was written" }
    }
    let rows = (lock promote)
    print $"wrote ($rows | length) entries to (lock committed_path)"
}

def "main lock show" [] {
    lock effective | sort-by image tag arch
}

# Copies the images published for this machine's architecture (or --arch) into the registry, under the names and tags the table
# gives, so a cluster can start without building anything. Reads `<from>/polar-<image>:<tag>-<arch>` (see `publish`), then also
# copies the third-party images (--skip-mirrors leaves those). Neo4j is not published: build it (README, step 3).
def "main import" [
    ...names: string             # only these images (default: every image this repository builds)
    --from: string               # where the images are published, e.g. docker.io/<account>
    --arch: string = ""          # amd64 or arm64 (default: this machine's)
    --src-authfile: string = ""  # credentials for a private source
    --src-insecure               # the source serves plain HTTP or a certificate that is not trusted
    --skip-mirrors
] {
    if ($from | is-empty) { error make --unspanned { msg: "say where the images are published: --from docker.io/<account>" } }
    require_registry
    let want = if ($arch | is-empty) { arch } else { arch_of $arch }
    let rows = (published_rows (load_table) $names)
    let evict = (runtime) == "u7s" and (node_present)
    print $"importing ($rows | length) image\(s\) for ($want) from ($from) into (registry)"
    let auth = (if ($src_authfile | is-empty) { [] } else { ["--src-authfile" $src_authfile] }) ++ (if $src_insecure { ["--src-tls-verify=false"] } else { [] })
    mut failures = []
    for it in ($rows | enumerate) {
        let row = $it.item
        let ref = (published_ref $from $row $want)
        print $"  [($it.index + 1)/($rows | length)] ($ref)"
        let r = (do { ^skopeo copy --retry-times 3 --format v2s2 ...$auth --dest-tls-verify=false $"docker://($ref)" $"docker://(registry)/(dest $row)" } | complete)
        if $r.exit_code != 0 {
            let why = ($r.stderr | lines | last 1 | str join "")
            print $"      FAILED: ($why)"
            $failures = ($failures | append { image: $row.image, error: $why })
        } else {
            record_digest $row
            if $evict { evict_row $row }
        }
    }
    if not $skip_mirrors and ($names | is-empty) {
        let mirrors = (load_table | where kind == "mirror")
        print $"copying ($mirrors | length) third-party image\(s\) from their registries"
        for row in $mirrors {
            let err = (try { mirror_row $row; null } catch {|e| $e.msg })
            if $err != null { $failures = ($failures | append { image: $row.image, error: $err }) }
        }
    }
    if ($failures | is-not-empty) {
        print ""
        print $"($failures | length) failed:"
        for f in $failures { print $"  ($f.image): ($f.error)" }
        error make --unspanned { msg: $"($failures | length) image\(s\) could not be imported. Were they published for ($want)?" }
    }
    print "imported. Neo4j is not published: build it (README, step 3), then `just images-push neo4j`."
}

# Builds the images for an architecture (cross-built with Nix when it is not this machine's) and pushes each to
# `<to>/polar-<image>:<tag>-<arch>`, which `import` reads. The account's login comes from --authfile (a skopeo/docker auth file)
# or skopeo's usual places; it is never printed.
def "main publish" [
    ...names: string             # only these images (default: every image this repository builds)
    --to: string                 # e.g. docker.io/<account>
    --arch: string = ""          # amd64 or arm64 (default: this machine's)
    --authfile: string = ""
] {
    if ($to | is-empty) { error make --unspanned { msg: "say where to publish: --to docker.io/<account>" } }
    let want = if ($arch | is-empty) { arch } else { arch_of $arch }
    let system = (system_of $want)
    let rows = (published_rows (load_table) $names)
    let auth = if ($authfile | is-empty) { [] } else { ["--dest-authfile" $authfile] }
    mut failures = []
    for it in ($rows | enumerate) {
        let row = $it.item
        let ref = (published_ref $to $row $want)
        print $"  [($it.index + 1)/($rows | length)] ($row.image) -> ($ref)"
        let err = (try {
            # What this repository builds goes up from the archive it built; a mirror goes up as its upstream's image for the architecture.
            let source = if $row.kind == "mirror" { $"docker://($row.upstream)" } else { $"docker-archive:(archive $row $system)" }
            let platform = if $row.kind == "mirror" { ["--override-os" "linux" "--override-arch" $want] } else { [] }
            let r = (do { ^skopeo copy --retry-times 3 --format v2s2 ...$platform ...$auth $source $"docker://($ref)" } | complete)
            if $r.exit_code != 0 { error make --unspanned { msg: ($r.stderr | lines | last 1 | str join "") } }
            null
        } catch {|e| $e.msg })
        if $err != null {
            print $"      FAILED: ($err)"
            $failures = ($failures | append { image: $row.image, error: $err })
        }
    }
    if ($failures | is-not-empty) {
        print $"($failures | length) of ($rows | length) failed:"
        for f in $failures { print $"  ($f.image): ($f.error)" }
        error make --unspanned { msg: $"($failures | length) image\(s\) were not published" }
    }
    print $"published ($rows | length) image\(s\) for ($want) to ($to)"
}

# List the images in the table.
def "main list" [] {
    load_table | select image tag kind
}
