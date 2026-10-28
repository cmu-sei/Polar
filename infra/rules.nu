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

# infra/rules.nu
#
# Checks on what a workload resolves to, run by `resolve.nu check`. Each returns the problems
# it finds (an empty list means none).
#
# Whoever owns a fact owns the rule about it:
#
#   - What the agent's code needs of its configuration, which variables, what values, what must
#     hold between them (the health actor's rejuvenation room, the probe against the tick),
#     is the code's, so the rules are Rust. The resolver runs the agent's own `--check` on the
#     document the workload will be given (resolve.nu, `agent_check`). There is no second copy
#     of those rules here to drift.
#   - What only the deployment knows is the deployment's, so it is here: the pod's termination
#     grace period against the shutdown budget, a Kubernetes fact no agent knows.

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

# What is wrong with a Deployment whose pod runs polar-init, which restarts in place (docs/architecture/deployments.md,
# step 4): one pod (`replicas: 1`) replaced only once the old one is gone (`Recreate`), never two of one identity at once;
# and its workload containers kept by the pod (`restartPolicy: Always`) and restarting the whole pod on every exit
# (`RestartAllContainers`, `NotIn []`), so that every ending runs polar-init again. A Deployment without polar-init
# (Keycloak, Jaeger) is not one of these.
export def in_place_problems [docs: list]: nothing -> list<string> {
    $docs
    | where {|d| ($d.kind? | default "") == "Deployment" }
    | where {|d| ($d.spec?.template?.spec?.initContainers? | default [] | any {|c| $c.name == "polar-init" }) }
    | each {|d|
        let name = $d.metadata.name
        let pod = $d.spec.template.spec
        mut p = []
        if ($d.spec.replicas? | default 1) != 1 {
            $p = ($p | append $"Deployment ($name) has ($d.spec.replicas) replicas: one pod of an identity at a time")
        }
        if ($d.spec.strategy?.type? | default "RollingUpdate") != "Recreate" {
            $p = ($p | append $"Deployment ($name) does not roll out by Recreate: a rolling update starts the new pod before the old one ends")
        }
        for c in ($pod.containers? | default []) {
            if ($c.restartPolicy? | default "") != "Always" {
                $p = ($p | append $"Deployment ($name)'s container ($c.name) has no restartPolicy Always: its restart rule needs one")
            }
            let rules = ($c.restartPolicyRules? | default [])
            if not ($rules | any {|r| $r.action? == "RestartAllContainers" }) {
                $p = ($p | append $"Deployment ($name)'s container ($c.name) has no RestartAllContainers rule: an ending would not run polar-init again")
            } else if not ($rules | any {|r| $r.action? == "RestartAllContainers" and $r.exitCodes?.operator? == "NotIn" and ($r.exitCodes?.values? | default []) == [] }) {
                $p = ($p | append $"Deployment ($name)'s container ($c.name) restarts its pod on some exits, not on every exit")
            }
        }
        $p
    }
    | flatten
}

# The pod spec of a rendered object, or null when it has none: a Pod's own, a workload's template, a CronJob's.
def pod_spec_of [d: record] {
    match ($d.kind? | default "") {
        "Pod" => ($d | get -o spec)
        "CronJob" => ($d | get -o spec.jobTemplate.spec.template.spec)
        "Job" | "Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet" => ($d | get -o spec.template.spec)
        _ => null
    }
}

# Whether an image reference names something that can change under the same name: a tag that is `latest` or absent, with no digest.
export def is_mutable_ref [ref: string]: nothing -> bool {
    if ($ref | str contains "@sha256:") { return false }
    let last = ($ref | split row "/" | last)
    if not ($last | str contains ":") { return true }
    ($last | split row ":" | last) == "latest"
}

# Containers whose image can change under its name but whose pull policy lets a node keep the first copy it pulled. `IfNotPresent` (or
# `Never`) on `latest` means a node that has run an image once never sees a rebuilt one: the pod runs old code, and what it does looks
# like a bug in the new code. Kubernetes' own default for such a tag is Always; an unset policy is left to that default.
export def pull_policy_problems [docs: list]: nothing -> list<string> {
    $docs | each {|d|
        let spec = (pod_spec_of $d)
        if $spec == null { return [] }
        (($spec | get -o containers | default []) ++ ($spec | get -o initContainers | default []))
        | where {|c|
            let policy = ($c | get -o imagePullPolicy)
            ($policy != null) and ($policy != "Always") and (is_mutable_ref ($c | get -o image | default ""))
        }
        | each {|c|
            $"(($d.kind)) (($d.metadata?.name? | default '?')): container ($c.name) runs ($c.image) with imagePullPolicy ($c.imagePullPolicy). That tag moves, so a node keeps the first copy it pulled and never runs a rebuilt image; use Always"
        }
    } | flatten
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

# Where a workload gets its certificates: cert-issuer itself (only the bootstrap identities, the ones that
# cannot be issued through the credentials agent because they are what it needs to run) or the credentials
# agent. `records` have who, issuer and bootstrap. Until the floor is enforced a non-bootstrap workload that
# still asks cert-issuer is pending migration, listed and not an error; once cert-issuer refuses it, it is an
# error, because it could not start. Returns { errors, pending }.
export def issuer_problems [records: list, enforced: bool]: nothing -> record {
    mut errors: list<string> = []
    mut pending: list<string> = []
    for r in $records {
        if not ($r.issuer in ["cert-issuer" "credentials-agent"]) {
            $errors = ($errors | append $"($r.who): issuer '($r.issuer)' must be cert-issuer or credentials-agent")
        } else if $r.bootstrap and $r.issuer != "cert-issuer" {
            $errors = ($errors | append $"($r.who): a bootstrap identity is issued by cert-issuer, since it is what the credentials agent needs to run")
        } else if $r.issuer == "cert-issuer" and (not $r.bootstrap) {
            let m = $"($r.who) asks cert-issuer directly and is not a bootstrap identity"
            if $enforced { $errors = ($errors | append $m) } else { $pending = ($pending | append $m) }
        }
    }
    { errors: $errors, pending: $pending }
}

# What a workload's certificates carry of its scaffold: the declared and family topics (`topics`), not the implied
# ones, which follow from its identity and its groups; and its `groups`, which decide those. Sorted, each once.
# What is wrong with a pod's certificates when its keys are the custody daemon's: it holds one key of each type, client
# and server, so two certificates of one type cannot both be its (docs/architecture/key-custody.md, step 4); and with it,
# the token buys only its custodian's certificate ("Part A's bootstrap: the custodian"), so none beside the daemon's has a
# key file, as polar-init's `custody_for` also says (the maintainer, 2026-10-28). `specs` are polar-init's `--cert`
# values, `type:dir:algorithm:names`; with none whose algorithm is `custody`, there is nothing to say.
export def custody_cert_problems [specs: list<string>]: nothing -> list<string> {
    let parts = ($specs | each {|c| $c | split row ":" })
    let custody = ($parts | where {|p| ($p | get -o 2) == "custody" })
    if ($custody | is-empty) { return [] }
    let two = ($custody | group-by {|p| $p.0 } | items {|kind, certs|
        if ($certs | length) > 1 {
            $"two ($kind) certificates under the custody daemon \(($certs | each {|p| $p.1 } | str join ', ')\): it holds one key of each type"
        }
    } | compact)
    let files = ($parts | where {|p| ($p | get -o 2) != "custody" } | each {|p|
        $"the ($p.0) certificate into ($p.1) has a key file beside the custody daemon's: with the daemon, the token buys only its custodian's certificate"
    })
    $two ++ $files
}

# The label and taint key of a node pool: nix-usernetes' (`cluster.dhall`, `poolKey`), where each worker is one pool,
# labelled `polar.dev/pool=<pool>`, and a dedicated pool's node is also tainted `polar.dev/pool=<pool>:NoSchedule`
# (docs/architecture/placement.md).
export const POOL_KEY = "polar.dev/pool"

# Where a workload of `pool` goes, as the pod's template takes it: `{ key, pool }` when the target declares the pool
# (`declared` is its cluster's `placement`, `{ pools: [...] }`, or null), so the pod gets the pool's selector and the
# toleration of its taint; null for the open pool's (`pool` null), on a target that declares no placement, or for a pool
# the target lacks (which `placement_problems` reports).
export def placement_of [declared: any, pool: any]: nothing -> any {
    if $declared == null or $pool == null { return null }
    if $pool in ($declared.pools? | default []) { { key: $POOL_KEY, pool: $pool } } else { null }
}

# What is wrong with `who`'s placement: on a target that declares placement, a pool it does not declare, which would
# otherwise land on the open pool unseen.
export def placement_problems [declared: any, pool: any, who: string]: nothing -> list<string> {
    if $declared == null or $pool == null { return [] }
    let pools = ($declared.pools? | default [])
    if $pool in $pools { [] } else {
        [$"($who) asks for the pool '($pool)', which this target does not declare \(it declares: ($pools | str join ', ')\)"]
    }
}

# What is wrong with a pod whose polar-init is asked to fetch the escrow key (`POLAR_INIT_ESCROW_BOOTSTRAP`, from the
# `encryption` capability) when it has no custody daemon: only the daemon's custodian fetches the escrow key
# (docs/architecture/key-custody.md, step 9), and polar-init would stop at once. `custody` is whether the pod has the
# daemon; `init_env` is polar-init's environment, as records with `name` and `value`. Decided by the maintainer,
# 2026-10-28: the render refuses it, and key files stay a supported mode for agents that do not encrypt.
export def escrow_problems [custody: bool, init_env: list]: nothing -> list<string> {
    let escrow = ($init_env | any {|e| $e.name == "POLAR_INIT_ESCROW_BOOTSTRAP" and ($e.value? | default "") == "true" })
    if $escrow and (not $custody) {
        ["it encrypts, and its escrow key is fetched only by the custody daemon's custodian, which this pod does not have: turn custody on for it, or leave it out of this target"]
    } else { [] }
}

def own_topics [r: record]: nothing -> record {
    let own = ($r.topics? | default [] | where {|t| $t.from != "implied" })
    {
        publish: ($own | where access == "publish" | get name | uniq | sort)
        subscribe: ($own | where access == "subscribe" | get name | uniq | sort)
        groups: ($r.groups? | default [] | uniq | sort)
    }
}

# The authority model's base facts (docs/architecture/authority-model.md, "The proposal" and "Build order", slice 3): every
# workload, whoever issues it, by its one identity `<workload>.<namespace>.serviceaccount.<domain>` (never its token's
# subject), with its issuer, whether it is a bootstrap service, its certificate's groups, and the whole set of topics its
# scaffold prints: declared, family and implied, each with what it says about causes. The credentials agent derives the
# paths from these. A workload's service account is its name ("The workload name is an agent's one name",
# policy-enforcement.md), so one that is not would put a second name in the model, and is an error. `records` are as for
# floor_entries.
export def model_facts [records: list, domain: string]: nothing -> record {
    let misnamed = ($records | where {|r| $r.service_account != $r.who })
    if ($misnamed | is-not-empty) {
        let named = ($misnamed | each {|r| $"($r.who) \(service account ($r.service_account)\)" } | str join ", ")
        error make { msg: $"its service account is not its name, so its identity would name another workload: ($named)" }
    }
    let workloads = ($records | each {|r|
        {
            workload: $r.who
            identity: $"($r.who).($r.namespace).serviceaccount.($domain)"
            issuer: $r.issuer
            bootstrap: $r.bootstrap
            groups: ($r.groups? | default [] | uniq | sort)
            topics: ($r.topics? | default [] | sort-by name access)
        }
    } | sort-by workload)
    { format: 1, workloads: $workloads }
}

# The entries of cert-issuer's floor, from what the catalog says each workload is issued (the `--cert` specs
# `kind:dir:algorithm:names`). A bootstrap identity gets an entry that lets its token alone be signed, for the one
# type it is issued and the names it carries. A workload on the credentials agent that is issued a server
# certificate gets a `vouchedOnly` entry, so cert-issuer checks the names the credentials agent states for it and
# never signs it on its token alone. Anything else has no entry. A bootstrap service's entry also carries the topics its
# scaffold declares (and its families, expanded), for cert-issuer to write into its certificates; implied topics are not
# written, since they follow from the identity (docs/architecture/policy-enforcement.md, stage 2). A workload on the
# credentials agent has its topics in the credentials agent's floor instead. `records` may carry `topics`, as the
# scaffold prints them. Returns { entries, problems }.
export def floor_entries [records: list]: nothing -> record {
    mut entries: list<any> = []
    mut problems: list<string> = []
    for r in $records {
        let parsed = ($r.certs | each {|c|
            let p = ($c | split row ":")
            { kind: ($p | get 0), names: (($p | get -o 3 | default "") | split row "," | where {|n| $n != "" }) }
        })
        let workload = $"system:serviceaccount:($r.namespace):($r.service_account)"
        if $r.bootstrap {
            let kinds = ($parsed | get kind | uniq)
            if ($kinds | length) != 1 {
                $problems = ($problems | append $"($r.who): a bootstrap identity is written as one certificate type, and this one is issued ($kinds | str join ' and ')")
            } else {
                let own = (own_topics $r)
                $entries = ($entries | append {
                    workload: $workload
                    certType: ($kinds | first | str uppercase)
                    names: ($parsed | get names | flatten | uniq)
                    vouchedOnly: false
                    publish: $own.publish
                    subscribe: $own.subscribe
                    groups: $own.groups
                })
            }
        } else if $r.issuer == "credentials-agent" {
            let servers = ($parsed | where kind == "server")
            if ($servers | is-not-empty) {
                $entries = ($entries | append {
                    workload: $workload
                    certType: "SERVER"
                    names: ($servers | get names | flatten | uniq)
                    vouchedOnly: true
                    publish: []
                    subscribe: []
                    groups: []
                })
            }
        }
    }
    { entries: $entries, problems: $problems }
}
