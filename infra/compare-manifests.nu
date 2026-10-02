#!/usr/bin/env nu
# infra/compare-manifests.nu
#
# Compares two directories of rendered manifests by meaning, not by text.
#
# Every document in every .yaml file is keyed by Kind/namespace/name, so a
# document that moved to another file, or changed position in a multi-document
# file, still matches. Within a document, lists of named things (env, volumes,
# mounts, containers) are compared in name order and polar-init's args as
# sorted flag/value pairs. Use it to check that a render from the catalog
# says the same thing as the render from the charts it replaces.
#
#   cp -r manifests manifests.old            # a render from the old pipeline
#   nu infra/render.nu local-minimal         # a render from the new one
#   nu infra/compare-manifests.nu manifests.old manifests kube-observer kube-consumer
#
# The names after the directories keep only documents whose key contains one
# of them (none: compare everything). Prints one line per difference. Exit
# status is 1 if there are any.

# Sort anything whose order carries no meaning.
def norm [x: any] {
    let t = ($x | describe)
    if ($t | str starts-with "record") {
        $x | items {|k, v|
            { ($k): (if $k == "args" { norm_args $v } else { norm $v }) }
        } | reduce --fold {} {|it, acc| $acc | merge $it }
    } else if ($t | str starts-with "list") or ($t | str starts-with "table") {
        let items = ($x | each {|i| norm $i })
        let all_named = (
            ($items | is-not-empty)
            and ($items | all {|i|
                ($i | describe | str starts-with "record") and ("name" in ($i | columns))
            })
        )
        if $all_named { $items | sort-by name } else { $items }
    } else {
        $x
    }
}

# ["--cert", "a", "--check-dep-cert", "b"] -> sorted pairs, still flat.
def norm_args [args: any] {
    if ($args | describe | str starts-with "list") and (($args | length) mod 2 == 0) {
        $args | window 2 --stride 2 | each {|p| $"($p.0) ($p.1)" } | sort
    } else {
        $args
    }
}

# Every leaf as {path, value}.
def leaves [x: any, prefix: string] {
    let t = ($x | describe)
    if ($t | str starts-with "record") {
        if ($x | columns | is-empty) { return [{ path: $prefix, value: "{}" }] }
        $x | items {|k, v| leaves $v $"($prefix).($k)" } | flatten
    } else if ($t | str starts-with "list") or ($t | str starts-with "table") {
        if ($x | is-empty) { return [{ path: $prefix, value: "[]" }] }
        $x | enumerate | each {|e|
            let item = $e.item
            let label = if ($item | describe | str starts-with "record") and ("name" in ($item | columns)) { $item.name } else { $e.index | into string }
            leaves $item $"($prefix)[($label)]"
        } | flatten
    } else {
        [{ path: $prefix, value: ($x | to nuon) }]
    }
}

# Every document of every .yaml file in a directory, as {key, path, value} leaves.
def load_dir [dir: string, filters: list<string>] {
    glob ($dir | path join "*.yaml")
    | each {|f|
        open --raw $f
        | split row --regex '(?m)^---\s*$'
        | where {|d| ($d | str trim | str length) > 0 }
        | each {|d| $d | from yaml }
        | where {|d| ($d | describe | str starts-with "record") and ($d | get -o kind) != null }
    }
    | flatten
    | each {|d|
        let key = $"($d.kind)/($d.metadata?.namespace? | default '-')/($d.metadata.name)"
        { key: $key, doc: (norm $d) }
    }
    | where {|d| ($filters | is-empty) or ($filters | any {|f| $d.key | str contains $f }) }
    | each {|d| leaves $d.doc "" | each {|l| { key: $d.key, path: $l.path, value: $l.value } } }
    | flatten
}

def main [old_dir: string, new_dir: string, ...names: string] {
    let a = (load_dir $old_dir $names)
    let b = (load_dir $new_dir $names)
    let id = {|r| $"($r.key)($r.path)" }
    let a_ids = ($a | each {|r| do $id $r })
    let b_ids = ($b | each {|r| do $id $r })

    let keys = (($a | get key) ++ ($b | get key) | uniq | sort)
    if ($keys | is-empty) { print "no documents matched"; exit 1 }

    mut count = 0
    for k in $keys {
        let ra = ($a | where key == $k)
        let rb = ($b | where key == $k)
        if ($ra | is-empty) { print $"($k): only in ($new_dir)"; $count += 1; continue }
        if ($rb | is-empty) { print $"($k): only in ($old_dir)"; $count += 1; continue }

        let pa = ($ra | get path)
        let pb = ($rb | get path)
        let removed = ($ra | where {|r| not ($r.path in $pb) })
        let added = ($rb | where {|r| not ($r.path in $pa) })
        let changed = ($ra | where {|r| $r.path in $pb } | where {|r| ($rb | where path == $r.path | first | get value) != $r.value })

        if ([$removed $added $changed] | all {|l| $l | is-empty }) {
            print $"($k): same"
        } else {
            print $"($k):"
            for r in $removed { print $"    - ($r.path) = ($r.value)"; $count += 1 }
            for r in $added { print $"    + ($r.path) = ($r.value)"; $count += 1 }
            for r in $changed {
                let nv = ($rb | where path == $r.path | first | get value)
                print $"    ~ ($r.path): ($r.value) -> ($nv)"
                $count += 1
            }
        }
    }
    if $count > 0 { exit 1 }
}
