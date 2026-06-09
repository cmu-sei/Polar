# ---------------------------------------------------------------------------
# Content hashing
# ---------------------------------------------------------------------------

export def content-hash-file [path: string]: nothing -> string {
    open $path --raw | hash sha256 | $"sha256:($in)"
}

export def content-hash-dir [dir: string]: nothing -> string {
    let git_check = try {
        git -C $dir rev-parse --git-dir | complete
    } catch { {exit_code: 1} }
    if $git_check.exit_code == 0 {
        let h = git -C $dir rev-parse "HEAD^{tree}" | str trim
        $"sha256:($h)"
    } else {
        glob $"($dir)/**/*"
        | where { ($in | path type) == "file" }
        | sort
        | each { open $in --raw | hash sha256 }
        | str join "\n"
        | hash sha256
        | $"sha256:($in)"
    }
}

export def content-hash [path: string]: nothing -> string {
    if ($path | path type) == "dir" {
        content-hash-dir $path
    } else {
        content-hash-file $path
    }
}

export def git-commit-sha [dir: string]: nothing -> record {
    let result = try {
        git -C $dir rev-parse HEAD | complete
    } catch { {exit_code: 1} }
    if $result.exit_code == 0 {
        {
            available: true
            sha: ($result.stdout | str trim)
        }
    } else {
        {available: false, sha: ""}
    }
}

# Hash a directory tree via git tree hash if available, else sorted file hashes.
export def tree-hash [dir: string]: nothing -> string {
    let git_check = try {
        git -C $dir rev-parse --git-dir | complete
    } catch { {exit_code: 1} }
    if $git_check.exit_code == 0 {
        let h = git -C $dir rev-parse "HEAD^{tree}" | str trim
        $"sha256:($h)"
    } else {
        glob $"($dir)/**/*"
        | where { $in | path type | $in == "file" }
        | sort
        | each { open $in --raw | hash sha256 }
        | str join "\n"
        | hash sha256
        | $"sha256:($in)"
    }
}
