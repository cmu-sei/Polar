export module cargo.nu
export module cassini.nu
export module dhall.nu
export module events.nu
export module hashing.nu
export module logging.nu
export module oci.nu
export module sbom.nu
export module scanning.nu
export module state.nu

# ANSI helpers — used only for static analysis terminal output.
export def print-green [msg: string] { $"(ansi green)($msg)(ansi reset)" }
export def red [msg: string] { $"(ansi red)($msg)(ansi reset)" }
export def yellow [msg: string] { $"(ansi yellow)($msg)(ansi reset)" }
export def bold [msg: string] { $"(ansi attr_bold)($msg)(ansi reset)" }

# ---------------------------------------------------------------------------
# Timing
# ---------------------------------------------------------------------------

export def elapsed-ms [start: datetime]: nothing -> int {
    (((date now) - $start) / 1_000_000) | into int
}

export def commit-sha []: nothing -> string {
    return (^git rev-parse HEAD | str trim)
}

export def commit-short-sha []: nothing -> string {
    return (^git rev-parse HEAD | str trim | str substring 0..8)
}
