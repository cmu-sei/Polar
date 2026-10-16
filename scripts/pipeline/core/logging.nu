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

# Logging module for CI/CD pipeline scripts.
#
# Thin wrapper around nushell's `std/log`, kept so the rest of the pipeline
# can keep calling `log-info` / `log-warn` / `log-error` / `log-debug` with
# the same `--component` flag as before. All level-filtering, formatting,
# and stream handling is now delegated to `std/log`; we just tighten
# NU_LOG_FORMAT / NU_LOG_DATE_FORMAT and fold --component into the message
# body (std/log's format string has no slot for a component tag).
#
# Behavioral changes vs. the old hand-rolled implementation:
#
#   - std/log writes to STDERR (via `print --stderr`), not stdout. If
#     anything downstream was capturing stdout expecting log lines mixed
#     in, that breaks now (this is the correct direction to break in).
#
#   - `log-debug` is now silenced unless $env.NU_LOG_LEVEL is "debug" (or
#     a numeric value <= 10). The old log-debug always printed. Set
#     NU_LOG_LEVEL=debug in CI when you need verbose output.
#
#   - The level tags change from the old literal INFO/WARN/ERROR/DEBUG
#     strings to whatever std/log's built-in %LEVEL% prefixes are for your
#     nushell version (commonly INF/WRN/ERR/DBG/CRT). If any downstream
#     tooling greps log output for "[WARN]" etc., update those patterns —
#     and pin/verify this against your actual nushell version, since
#     NU_LOG_FORMAT internals have changed across releases (see
#     nushell/nushell#14412).
#
# The env vars below are set via `export-env`, so they take effect globally
# for the whole script/session the first time any module does
# `use logging.nu` — not just within this module's scope. That's intentional:
# every script in the pipeline that touches std/log, directly or through this
# wrapper, gets the same wire format. Note also that std/log does not honor
# $env.config.use_ansi_coloring (nushell/nushell#13317), so if you pipe
# output to a plain-text file for archival you'll get raw ANSI escapes in it
# unless you strip %ANSI_START%/%ANSI_STOP% from NU_LOG_FORMAT yourself.

use std/log

export-env {
    # %DATE%, %LEVEL%, %MSG%, %ANSI_START%, %ANSI_STOP% are the only
    # placeholders std/log understands.
    $env.NU_LOG_FORMAT = "%ANSI_START%%DATE% [%LEVEL%] %MSG%%ANSI_STOP%"
    $env.NU_LOG_DATE_FORMAT = "%Y-%m-%d %H:%M:%S%.3f"
}

# Fold an optional --component tag into the message body.
def tag-component [msg: string, component: string] {
    if ($component | is-empty) {
        $msg
    } else {
        $"($component) — ($msg)"
    }
}

export def log-info [msg: string, --component: string = ""] {
    log info (tag-component $msg $component)
}

export def log-warn [msg: string, --component: string = ""] {
    log warning (tag-component $msg $component)
}

export def log-error [msg: string, --component: string = ""] {
    log error (tag-component $msg $component)
}

export def log-debug [msg: string, --component: string = ""] {
    log debug (tag-component $msg $component)
}
