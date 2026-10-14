#!/usr/bin/env nu
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
