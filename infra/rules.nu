#!/usr/bin/env nu
# infra/rules.nu
#
# Rules about the health and shutdown settings a workload resolves to. Each is a
# pure function of a record that returns the problems it finds (an empty list means
# none), so `resolve.nu check` can run them over every workload and the tests in
# infra/tests/rules.nu can run them over made-up ones.
#
# Why they exist: the agent's own code checks these when it starts (polar-health's
# HealthConfig::validate and the builder's budget check) and refuses to run on a bad
# combination. That is the authority. Checking the same things here moves the failure
# to render time, before a pod exists, instead of leaving it as a crash loop.
#
# They mirror the Rust rules; they do not share them. The formulas exist in both
# places, and the numbers they depend on are tied together by a test in polar-health
# (tests/catalog.rs) that reads infra/config/kinds.nuon and compares it with the Rust
# defaults, so changing one side without the other fails a build.

# Problems with a workload's `health` settings. `h` has tick_secs, expiry_secs,
# rejuvenation_secs, stale_secs and jitter_window_secs.
export def health_problems [who: string, h: record]: nothing -> list<string> {
    mut problems: list<string> = []

    if $h.tick_secs < 1 {
        $problems = ($problems | append $"($who): health.tick_secs must be at least 1, got ($h.tick_secs)")
    }
    if $h.expiry_secs <= 0 {
        $problems = ($problems | append $"($who): health.expiry_secs must be positive, got ($h.expiry_secs)")
    }

    if $h.rejuvenation_secs <= $h.expiry_secs {
        $problems = ($problems | append $"($who): health.rejuvenation_secs \(($h.rejuvenation_secs)\) must be greater than health.expiry_secs \(($h.expiry_secs)\)")
    } else {
        # A rejuvenation has to finish before the certificate reaches the hard floor.
        # The slowest case is a tick to notice the window and check the dependencies,
        # the longest jitter, and a tick to act on it: two ticks plus the jitter window.
        let needed = (2 * $h.tick_secs + $h.jitter_window_secs)
        let room = ($h.rejuvenation_secs - $h.expiry_secs)
        if $room < $needed {
            $problems = ($problems | append (
                $"($who): a rejuvenation can take ($needed)s \(two ticks of ($h.tick_secs)s plus the ($h.jitter_window_secs)s jitter window\), "
                + $"but health.rejuvenation_secs \(($h.rejuvenation_secs)\) minus health.expiry_secs \(($h.expiry_secs)\) leaves ($room)s, "
                + "so the certificate could reach its hard expiry floor before the agent could rejuvenate"
            ))
        }
    }

    # The probe must tolerate one slow tick, or it calls a healthy agent dead.
    if $h.stale_secs <= (2 * $h.tick_secs) {
        $problems = ($problems | append $"($who): health.stale_secs \(($h.stale_secs)\) must exceed twice health.tick_secs \(($h.tick_secs)\), or the liveness probe would call the agent dead after a single slow tick")
    }

    $problems
}

# Problems with a workload's `shutdown` settings. `s` has budget_secs,
# grace_margin_secs and the derived termination_grace_secs.
export def shutdown_problems [who: string, s: record]: nothing -> list<string> {
    mut problems: list<string> = []

    if $s.budget_secs < 1 {
        $problems = ($problems | append $"($who): shutdown.budget_secs must be a whole number of seconds, at least 1, got ($s.budget_secs)")
    }
    if $s.grace_margin_secs < 1 {
        $problems = ($problems | append $"($who): shutdown.grace_margin_secs must be at least 1, got ($s.grace_margin_secs)")
    }
    # Kubernetes sends SIGKILL when the grace period ends. The agent's own watchdog
    # exits at the budget, so the pod must be allowed to live longer than that.
    if $s.termination_grace_secs <= $s.budget_secs {
        $problems = ($problems | append $"($who): the termination grace period \(($s.termination_grace_secs)s\) must exceed the shutdown budget \(($s.budget_secs)s\), or Kubernetes would kill the pod before the agent's own watchdog could end it")
    }

    $problems
}
