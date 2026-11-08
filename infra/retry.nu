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

# infra/retry.nu
#
# For what up.nu does through a port-forward, or against a pod that is being replaced: the forward is
# reconnecting for a second or two, and trying again a moment later works. A failure that trying again cannot
# fix (a refusal) is marked final and stops at once.

# Runs `attempt` until it succeeds or `attempts` are used, waiting `pause` between tries (not after the last).
# `attempt` takes the try number, from 1, and returns `{ ok, final, detail }`. Returns `{ ok, tries, detail }`,
# where `detail` is what the last try said.
export def with_retries [attempts: int, pause: duration, attempt: closure] {
    mut tries = 0
    mut last = { ok: false, final: false, detail: "no attempt was made" }
    for n in 1..$attempts {
        $tries = $n
        $last = (do $attempt $n)
        if $last.ok or $last.final { break }
        if $n < $attempts { sleep $pause }
    }
    { ok: $last.ok, tries: $tries, detail: $last.detail }
}
