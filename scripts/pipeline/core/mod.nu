# ANSI helpers — used only for static analysis terminal output.
# TODO: It would also be interesting if log fns could use this
export def green [msg: string] { $"(ansi green)($msg)(ansi reset)" }
export def red [msg: string] { $"(ansi red)($msg)(ansi reset)" }
export def yellow [msg: string] { $"(ansi yellow)($msg)(ansi reset)" }
export def bold [msg: string] { $"(ansi attr_bold)($msg)(ansi reset)" }

# ---------------------------------------------------------------------------
# Timing
# ---------------------------------------------------------------------------

export def elapsed-ms [start: datetime]: nothing -> int {
    (((date now) - $start) / 1_000_000) | into int
}
