#!/usr/bin/env nu
# infra/rules.nu
#
# Checks on what a workload resolves to, run by `resolve.nu check`. Each returns the problems
# it finds (an empty list means none).
#
# Whoever owns a fact owns the rule about it:
#
#   - What the agent's code needs of its environment, which variables, what values, what must
#     hold between them (the health actor's rejuvenation room, the probe against the tick),
#     is the code's, so the rules are Rust. `environment_problems` runs the very function the
#     agent runs when it starts, through `polar-config-check`, on the environment the workload
#     will be given. There is no second copy of those rules here to drift.
#   - What only the deployment knows is the deployment's, so it is here: the pod's termination
#     grace period against the shutdown budget, a Kubernetes fact no agent knows.

const INFRA = (path self | path dirname)

# Prefixes of the variables an agent reads. The checker must see only what the workload is
# given, so any of these set in the caller's shell are unset for the check.
# What the checker is given for a variable the pod will get from a secret.
const SECRET_PLACEHOLDER = "from-secret"

const AGENT_ENV_PREFIXES = '^(POLAR_|GRAPH_|TLS_|BROKER_|CASSINI_)'

# The command that runs polar-config-check: the binary named by POLAR_CONFIG_CHECK if that is
# set (a prebuilt one, in CI), otherwise `cargo run`, which rebuilds it when the code changed.
# Not finding it is an error, never a skipped check: a check that quietly does not run is how
# the rules drift.
def checker []: nothing -> list<string> {
    let explicit = ($env | get -o POLAR_CONFIG_CHECK)
    if $explicit != null {
        [$explicit]
    } else {
        let manifest = ($INFRA | path dirname | path join "src" "agents" "Cargo.toml")
        ["cargo" "run" "--quiet" "--manifest-path" $manifest "--package" "polar-agent-builder" "--bin" "polar-config-check" "--"]
    }
}

# Problems with the environment a workload will be given, judged by the agent's own code.
# `entries` is the workload's resolved `env` list. One that comes from a secret has no value at
# render time, so it is given a placeholder: the check is of shape, and a variable the pod gets
# from a secret is present. The placeholder never reaches a message, since a secret is declared
# secret and is never shown.
export def environment_problems [who: string, entries: list]: nothing -> list<string> {
    let unset = (
        $env | columns
        | where {|c| ($c =~ $AGENT_ENV_PREFIXES) and ($c != "POLAR_CONFIG_CHECK") }
        | each {|n| ["-u" $n] }
        | flatten
    )
    let set = ($entries | each {|e| $"($e.name)=(if $e.value != null { $e.value } else { $SECRET_PLACEHOLDER })" })
    let command = (checker)

    let result = (^env ...$unset ...$set ...$command | complete)

    # 0: sound. 1: problems, one per stderr line. Anything else means the checker itself
    # failed (not built, bad usage), which is not a finding about the workload.
    if $result.exit_code == 0 {
        []
    } else if $result.exit_code == 1 {
        $result.stderr
        | lines
        | where {|l| $l starts-with "ERROR: " }
        | each {|l| $"($who): ($l | str replace 'ERROR: ' '')" }
    } else {
        error make --unspanned { msg: $"polar-config-check failed to run for ($who) \(exit ($result.exit_code)\): ($result.stderr)" }
    }
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
