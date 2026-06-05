export def workspace-root [--manifest-path: string = ""]: nothing -> string {
    if ($manifest_path | is-not-empty) {
        return ($manifest_path | path dirname)
    }
    let git_root = (^git rev-parse --show-toplevel | str trim)
    let default_manifest = ($git_root | path join "src/agents/Cargo.toml")
    if ($default_manifest | path exists) {
        $git_root | path join "src/agents"
    } else {
        error make { msg: $"could not find Cargo.toml at ($default_manifest)" }
    }
}
