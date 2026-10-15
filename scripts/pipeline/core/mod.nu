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
