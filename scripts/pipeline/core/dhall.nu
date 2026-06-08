# ===========================================================================
# Dhall rendering
#
# Renders .dhall files to .yaml in-place alongside their sources.
# Rendered files are not committed — they are produced at pipeline time
# and consumed immediately by kubectl/flux. The conf directory is rendered
# the same way as service directories; the distinction between "manifests"
# and "configuration" is a concern for the caller, not this function.
#
# Import paths in Dhall files are relative to the source file location,
# so rendering must happen in-place rather than into a separate output tree.
# ===========================================================================

const DHALL_COMPONENT = "dhall"
# Render all .dhall files in a single directory to .yaml in the same directory.
# Non-.dhall files are ignored. Returns a list of { src, out, success } records.
export def render-dhall-dir [dhall_dir: path]: nothing -> list<record> {
    let dhall_files = (
        ls $dhall_dir
        | where type == file
        | where { $in.name | str ends-with ".dhall" }
    )

    if ($dhall_files | is-empty) {
        log-warn $"no .dhall files found in ($dhall_dir), skipping" --component $DHALL_COMPONENT
        return []
    }

    $dhall_files | each {|file|
        let src      = $file.name
        let stem     = ($src | path basename | str replace --regex '\.dhall$' '')
        let yaml_out = ($dhall_dir | path join $"($stem).yaml")

        log-info $"converting: ($src | path basename) -> ($yaml_out | path basename)" --component $DHALL_COMPONENT

        dhall-to-yaml --documents --file $src | save --force $yaml_out

        let ok = ($env.LAST_EXIT_CODE == 0)
        if not $ok {
            log-error $"dhall-to-yaml failed for ($src)" --component $DHALL_COMPONENT
        }

        { src: $src, out: $yaml_out, success: $ok }
    }
}

# Render all .dhall files under every subdirectory of dhall_root.
# Each subdirectory is processed independently; files at the root level
# are not rendered (they are typically shared imports, not entrypoints).
# Returns a flat list of { src, out, success } records across all dirs.
#
# Fails fast on any conversion error — a partial render is worse than
# no render because kubectl apply on a half-rendered tree is unpredictable.
export def render-dhall-root [dhall_root: path]: nothing -> list<record> {
    if not ($dhall_root | path exists) {
        error make { msg: $"dhall root '($dhall_root)' does not exist" }
    }

    let subdirs = (
        ls $dhall_root
        | where type == dir
    )

    if ($subdirs | is-empty) {
        log-warn $"no subdirectories found under ($dhall_root)" --component $DHALL_COMPONENT
        return []
    }

    let results = ($subdirs | each {|dir|
        let name = ($dir.name | path basename)
        log-info $"rendering ($name)" --component $DHALL_COMPONENT
        let dir_results = (render-dhall-dir $dir.name)
        log-info $"($name): ($dir_results | length) file(s) rendered" --component $DHALL_COMPONENT
        $dir_results
    } | flatten)

    let failures = ($results | where success == false)
    if ($failures | is-not-empty) {
        let failed_files = ($failures | get src | each { path basename } | str join ", ")
        error make { msg: $"dhall rendering failed for: ($failed_files)" }
    }

    $results
}
