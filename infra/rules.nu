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

# Problems with how rendered Jobs replace their pods. `docs` is every rendered Kubernetes object.
#
# A Job must wait until a pod is fully gone before it creates the next (`Failed`). The default
# creates the replacement as soon as the old pod starts terminating, so a deleted or evicted pod
# overlaps with its replacement for the whole drain: two instances of one identity at once, with
# the same key, the same volumes. Every agent ends a pod by exiting non-zero, so a pod always
# finishes Failed and the stricter policy loses nothing.
export def job_replacement_problems [docs: list]: nothing -> list<string> {
    $docs
    | where {|d| ($d.kind? | default "") == "Job" }
    | where {|d| ($d.spec?.podReplacementPolicy? | default "") != "Failed" }
    | each {|d|
        let policy = ($d.spec?.podReplacementPolicy? | default "unset")
        $"Job ($d.metadata.name) has podReplacementPolicy ($policy), not Failed: a deleted pod would overlap with its replacement for the whole drain"
    }
}

# Every manifest the catalog rendered must be applied, and a workload's resources (its
# ConfigMap, token Secret, RBAC) before the Job that uses them: a Job whose ConfigMap is not there
# yet sits in ContainerCreating. `rendered` and `order` are file names.
export def apply_order_problems [rendered: list<string>, order: list<string>]: nothing -> list<string> {
    let missing = ($rendered | where {|f| not ($f in $order) } | each {|f|
        $"($f) is rendered but is not in the target's apply_order, so it would never be applied"
    })
    let misordered = ($rendered | where {|f| $f | str ends-with "-resources.yaml" } | each {|r|
        let job = ($r | str replace "-resources.yaml" ".yaml")
        let at_r = ($order | enumerate | where item == $r | get -o 0.index)
        let at_j = ($order | enumerate | where item == $job | get -o 0.index)
        if $at_r != null and $at_j != null and $at_r > $at_j {
            [$"($r) is applied after ($job); it must come before"]
        } else if $at_r != null and $at_j != null and $at_r == $at_j { [] } else { [] }
    } | flatten)
    $missing ++ $misordered
}
