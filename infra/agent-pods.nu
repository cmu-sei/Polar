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

# infra/agent-pods.nu
#
# Asking an agent about itself, in its own image, on the cluster Polar deploys to: `--scaffold` (what it takes) and
# `--check --config FILE` (its judgement of the document it will be given). A bring-up already has a cluster, so it
# needs no container runtime of its own for this, and the pod runs the artifact that deploys, pulled the way the
# nodes pull it. Each run gets a namespace of its own, deleted when the run ends, whether it succeeded or not; one
# left behind by an interrupted run is deleted by the next preflight (`stale_namespaces`).
#
# The pods have no network (a deny-all NetworkPolicy, where the network plugin enforces them), no ServiceAccount
# token, the restricted Pod Security profile and a read-only root with a scratch /tmp, and are never restarted. An
# image is named by the digest the registry this machine pushes to serves for it, so what runs is what was looked
# up, and a scaffold is cached under that digest.
#
# An agent prints its scaffold, or `ok:` for a sound document, on stdout, and each problem as an `ERROR:` line on
# stderr. A pod's log holds both streams, so a scaffold that is not JSON is refused and names the agent.

use cache.nu [cache_peek, cache_put, cached_text, content_key]
use digest.nu [registry_digest]

export const SCRATCH_LABEL = "polar.dev/scratch"
export const SCRATCH_VALUE = "agent-answers"
export const CREATED_ANNOTATION = "polar.dev/created-at"

# A scratch namespace older than this is left over from an interrupted run, not in use by another one.
export const STALE_AFTER = 1hr

# Where the agent finds the document it is asked to check.
export const DOCUMENT_PATH = "/polar-config.json"

# The user a pod runs as: the restricted profile refuses root, and an image need not name a user.
const POD_UID = 65532

# How long the pods of one run may take, the first pull of each image included.
def pod_timeout [] { $env.POLAR_AGENT_POD_TIMEOUT? | default "5min" | into duration }

# Waiting reasons that will not resolve by waiting.
const STUCK_REASONS = ["ErrImagePull" "InvalidImageName" "CreateContainerConfigError" "CreateContainerError" "RunContainerError"]

# A back-off is the kubelet waiting to pull again, and does not say why: after a throttled pull it resolves by itself, after a
# refused one it does not. It counts as stuck once it has lasted this long.
export const PULL_BACKOFF_LIMIT = 90sec

# ── What is applied (pure) ───────────────────────────────────────────────────

export def namespace_manifest [name: string, created: string] {
    {
        apiVersion: "v1"
        kind: "Namespace"
        metadata: {
            name: $name
            labels: { "polar.dev/scratch": $SCRATCH_VALUE, "pod-security.kubernetes.io/enforce": "restricted" }
            annotations: { "polar.dev/created-at": $created }
        }
    }
}

export def deny_all_policy [ns: string] {
    {
        apiVersion: "networking.k8s.io/v1"
        kind: "NetworkPolicy"
        metadata: { name: "deny-all", namespace: $ns }
        spec: { podSelector: {}, policyTypes: ["Ingress" "Egress"] }
    }
}

export def document_config_map [ns: string, name: string, content: string] {
    { apiVersion: "v1", kind: "ConfigMap", metadata: { name: $name, namespace: $ns }, data: { "config.json": $content } }
}

# A pod that runs `command` once in `image`. `command` is the image's own (`command_of_config`) followed by the question,
# because the images set a Cmd and no Entrypoint, and a pod's `args` alone would replace that Cmd. `config_map` names the
# document to mount at DOCUMENT_PATH, or is null; `variables` is a list of { name, value }.
export def agent_pod [ns: string, name: string, image: string, command: list<string>, config_map: any = null, variables: list = []] {
    let document = ($config_map != null)
    {
        apiVersion: "v1"
        kind: "Pod"
        metadata: { name: $name, namespace: $ns, labels: { "polar.dev/scratch": $SCRATCH_VALUE } }
        spec: {
            restartPolicy: "Never"
            automountServiceAccountToken: false
            enableServiceLinks: false
            activeDeadlineSeconds: ((pod_timeout) / 1sec | into int)
            securityContext: { runAsNonRoot: true, runAsUser: $POD_UID, runAsGroup: $POD_UID, seccompProfile: { type: "RuntimeDefault" } }
            containers: [{
                name: "agent"
                image: $image
                imagePullPolicy: "Always"
                command: $command
                env: $variables
                securityContext: { allowPrivilegeEscalation: false, readOnlyRootFilesystem: true, capabilities: { drop: ["ALL"] } }
                volumeMounts: ([{ name: "tmp", mountPath: "/tmp" }] ++ (if $document { [{ name: "document", mountPath: $DOCUMENT_PATH, subPath: "config.json", readOnly: true }] } else { [] }))
            }]
            volumes: ([{ name: "tmp", emptyDir: {} }] ++ (if $document { [{ name: "document", configMap: { name: $config_map } }] } else { [] }))
        }
    }
}

# The command of an image, from its configuration as `skopeo inspect --config` gives it: the Entrypoint, then the Cmd.
export def command_of_config [config: record] {
    let c = ($config.config? | default {})
    let command = (($c.Entrypoint? | default []) ++ ($c.Cmd? | default []))
    if ($command | is-empty) { error make --unspanned { msg: "the image names no command (no Entrypoint and no Cmd)" } }
    $command
}

# ── What comes back (pure) ───────────────────────────────────────────────────

# Where a pod is, from its status: { state: "running" | "backoff" | "done" | "stuck", exit, reason } ("backoff": a pull the kubelet will
# try again; the caller decides when that has gone on too long). `exit` is the container's exit code
# once it has one; a pod that failed without one (it ran out of time) is done with a null exit.
export def pod_outcome [pod: record] {
    let status = ($pod.status? | default {})
    let statuses = ($status.containerStatuses? | default [])
    let state = if ($statuses | is-empty) { {} } else { $statuses | first | get -o state | default {} }
    let terminated = ($state.terminated? | default null)
    if $terminated != null {
        return { state: "done", exit: $terminated.exitCode, reason: ($terminated.reason? | default "") }
    }
    let waiting = ($state.waiting? | default null)
    # The kubelet limits how fast it asks a registry (5 a second, a burst of 10), and a pull policy of Always asks for every container: a
    # burst of pods is told "pull QPS exceeded" and the kubelet tries again by itself. That resolves by waiting.
    let throttled = (($waiting.message? | default "") | str contains "pull QPS exceeded")
    if $waiting != null and ($waiting.reason? | default "") == "ImagePullBackOff" {
        return { state: "backoff", exit: null, reason: $"ImagePullBackOff: ($waiting.message? | default '')" }
    }
    if $waiting != null and ($waiting.reason? | default "") in $STUCK_REASONS and not $throttled {
        return { state: "stuck", exit: null, reason: $"($waiting.reason): ($waiting.message? | default '')" }
    }
    if ($status.phase? | default "") == "Failed" {
        return { state: "done", exit: null, reason: ($status.reason? | default "failed") }
    }
    { state: "running", exit: null, reason: "" }
}

# The scaffold `agent` printed, as text, or an error naming the agent.
export def scaffold_answer [agent: string, exit: any, logs: string] {
    if $exit != 0 {
        error make --unspanned { msg: $"($agent): `--scaffold` exited (if $exit == null { 'without an exit code' } else { $exit }): ($logs | lines | last 5 | str join ' | ')" }
    }
    let parsed = (try { $logs | from json } catch { null })
    if $parsed == null or ($parsed | describe | str starts-with "record") == false {
        error make --unspanned { msg: $"($agent): `--scaffold` printed something that is not a JSON scaffold; an agent must print only its scaffold: ($logs | lines | first 3 | str join ' | ')" }
    }
    $logs
}

# The problems `--check` found in `workload`'s document: its ERROR lines. A failure that names none is a problem too.
export def check_problems [workload: string, exit: any, logs: string] {
    if $exit == 0 { return [] }
    let named = ($logs | lines | where {|l| $l | str starts-with "ERROR:" } | each {|l| $"($workload): ($l | str replace 'ERROR: ' '')" })
    if ($named | is-not-empty) { return $named }
    [$"($workload): `--check` failed (if $exit == null { 'without an exit code' } else { $"with exit code ($exit)" }) and named no problem: ($logs | lines | last 3 | str join ' | ')"]
}

# The names of the scratch namespaces in `namespaces` (a `kubectl get namespaces -o json` items list, already selected by
# SCRATCH_LABEL) that are older than STALE_AFTER at `now`. One with no creation annotation is not ours to judge, so it is
# taken as stale only by Kubernetes' own creation time.
export def stale_namespaces [namespaces: list, now: datetime] {
    $namespaces | where {|n|
        let annotations = ($n.metadata?.annotations? | default {})
        let mine = ($annotations | transpose k v | where k == $CREATED_ANNOTATION | get -o 0.v)
        let created = ($mine | default ($n.metadata?.creationTimestamp? | default null))
        $created != null and (($now - ($created | into datetime)) > $STALE_AFTER)
    } | each {|n| $n.metadata.name }
}

# ── The registry ─────────────────────────────────────────────────────────────

# The registry this machine pushes to, and the images it holds under the names in images.nuon.
export def push_registry [] { $env.POLAR_REGISTRY? | default "localhost:5000" }

def tls_flags [] {
    let r = (push_registry)
    if ($r | str starts-with "localhost") or ($r | str starts-with "127.") { ["--tls-verify=false"] } else { [] }
}

# Where `image` is: as this machine pushed it (`pushed`), its digest there, and the name the nodes pull it by, which is
# a different address for the same registry (`node`, from the cluster's `registry`). An error when the registry does
# not have it.
export def locate [images: list, node_registry: string, image: string] {
    let row = ($images | where image == $image)
    if ($row | is-empty) { error make --unspanned { msg: $"image '($image)' has no row in infra/images.nuon" } }
    let pushed = $"(push_registry)/($image):(($row | first).tag)"
    let digest = (registry_digest $pushed)
    if $digest == null {
        error make --unspanned { msg: $"($pushed) is not in the registry, or the registry does not answer: load or push it first \(nu infra/images.nu import, or up.nu --dev\)" }
    }
    { image: $image, pushed: $pushed, digest: $digest, node: $"($node_registry)/($image)@($digest)" }
}

# The command of the image at `loc`, cached under its digest.
def image_command [loc: record] {
    let text = (cached_text "image-command" (content_key ["image-command" $loc.pushed $loc.digest]) {||
        let ref = $"(($loc.pushed | split row ':' | drop | str join ':'))@($loc.digest)"
        let r = (^skopeo inspect --config ...(tls_flags) $"docker://($ref)" | complete)
        if $r.exit_code != 0 { error make --unspanned { msg: $"cannot read the configuration of ($ref): ($r.stderr | str trim)" } }
        command_of_config ($r.stdout | from json) | to json --raw
    })
    $text | from json
}

# ── The cluster ──────────────────────────────────────────────────────────────

def apply [objects: list] {
    let r = ({ apiVersion: "v1", kind: "List", items: $objects } | to json | ^kubectl apply -f - | complete)
    if $r.exit_code != 0 { error make --unspanned { msg: $"the cluster refused the agent pods' objects: ($r.stderr | str trim)" } }
}

# Runs `body` with the name of a new scratch namespace, and deletes the namespace afterwards, whether `body` succeeded or not.
export def with_scratch_namespace [body: closure] {
    let ns = $"polar-agent-(random chars --length 6 | str lowercase)"
    apply [(namespace_manifest $ns (date now | format date '%+')) (deny_all_policy $ns)]
    let result = (try { { ok: true, value: (do $body $ns), error: null } } catch {|e| { ok: false, value: null, error: $e.msg } })
    ^kubectl delete namespace $ns --wait=false --ignore-not-found | complete | ignore
    if not $result.ok { error make --unspanned { msg: $result.error } }
    $result.value
}

# Deletes the scratch namespaces an interrupted run left behind. Returns their names.
export def delete_stale_namespaces [] {
    let r = (^kubectl get namespaces -l $"($SCRATCH_LABEL)=($SCRATCH_VALUE)" -o json | complete)
    if $r.exit_code != 0 { return [] }
    let stale = (stale_namespaces ($r.stdout | from json | get items) (date now))
    for ns in $stale { ^kubectl delete namespace $ns --wait=false --ignore-not-found | complete | ignore }
    $stale
}

# Applies `objects` (pods and what they mount) in `ns`, waits until every pod has finished, and returns
# { name, exit, logs } for each. A pod that cannot start (its image cannot be pulled) or that has not finished in time is an
# error that names it.
export def run_pods [ns: string, objects: list] {
    let names = ($objects | where kind == "Pod" | each {|p| $p.metadata.name })
    apply $objects
    let deadline = ((date now) + (pod_timeout))
    mut finished = []
    # When each pod's current pull back-off began.
    mut backoff_since = {}
    loop {
        let listed = (^kubectl get pods -n $ns -o json | complete)
        if $listed.exit_code != 0 { error make --unspanned { msg: $"cannot read the agent pods in ($ns): ($listed.stderr | str trim)" } }
        let seen = ($listed.stdout | from json | get items | each {|p| { name: $p.metadata.name, out: (pod_outcome $p) } } | where name in $names)
        let now = (date now)
        let before = $backoff_since
        $backoff_since = ($seen | where out.state == "backoff" | reduce --fold {} {|p, acc| $acc | insert $p.name ($before | get -o $p.name | default $now) })
        let began = $backoff_since
        let too_long = ($seen | where out.state == "backoff" | where {|p| ($now - ($began | get $p.name)) > $PULL_BACKOFF_LIMIT } | each {|p| $p | update out.state "stuck" })
        let stuck = (($seen | where out.state == "stuck") ++ $too_long)
        if ($stuck | is-not-empty) {
            error make --unspanned { msg: $"agent pod\(s\) cannot start: ($stuck | each {|s| $'($s.name) — ($s.out.reason)' } | str join '; ')" }
        }
        $finished = ($seen | where out.state == "done")
        if ($finished | length) == ($names | length) { break }
        if (date now) > $deadline {
            let waiting = ($names | where {|n| $n not-in ($finished | get name) })
            error make --unspanned { msg: $"agent pod\(s\) did not finish within (pod_timeout): ($waiting | str join ', ')" }
        }
        sleep 2sec
    }
    $finished | each {|f|
        let logs = (^kubectl logs -n $ns $f.name | complete)
        { name: $f.name, exit: $f.out.exit, logs: $logs.stdout }
    }
}

# ── Questions ────────────────────────────────────────────────────────────────

# Where an image's scaffold is cached: under its digest and the workloads its topic families are expanded for
# (POLAR_SERVED_WORKLOADS), since the scaffold depends on both.
export def scaffold_key [pushed: string, digest: string, served: string] {
    content_key ["scaffold" $pushed $digest $served]
}

# Writes `<workload>.json` to `dir` for each of `workloads` ({ name, image }): the scaffold its image prints. The images are
# run once each, together, in one scratch namespace; one whose scaffold is cached under its digest is not run. `node_registry`
# is the cluster's name for the registry. Each is asked with POLAR_SERVED_WORKLOADS set to `served`, the list the render
# writes into the documents, so an agent with a topic family prints it expanded into exact names. Returns how many images
# were run.
export def collect_scaffolds [workloads: list, dir: string, node_registry: string, images: list, served: string = ""] {
    mkdir $dir
    let located = ($workloads | get image | uniq | each {|i| locate $images $node_registry $i })
    let keyed = ($located | each {|l| $l | insert key (scaffold_key $l.pushed $l.digest $served) })
    let variables = (if ($served | is-empty) { [] } else { [{ name: "POLAR_SERVED_WORKLOADS", value: $served }] })
    let cached = ($keyed | each {|l| $l | insert text (cache_peek "scaffold" $l.key) })
    let missing = ($cached | where text == null)
    let answered = if ($missing | is-empty) { [] } else {
        with_scratch_namespace {|ns|
            let pods = ($missing | each {|l| agent_pod $ns $"scaffold-($l.image)" $l.node ((image_command $l) ++ ["--scaffold"]) null $variables })
            run_pods $ns $pods | each {|r|
                let l = ($missing | where {|m| $"scaffold-($m.image)" == $r.name } | first)
                let text = (scaffold_answer $l.image $r.exit $r.logs)
                cache_put "scaffold" $l.key $text
                { image: $l.image, text: $text }
            }
        }
    }
    let texts = (($cached | where text != null | select image text) ++ $answered)
    for w in $workloads {
        ($texts | where image == $w.image | first).text | save --force ($dir | path join $"($w.name).json")
    }
    $missing | length
}

# Asks each of `checks` ({ workload, image, document, secrets }) to judge its document, in one scratch namespace, and returns
# every problem found. `secrets` are the names of the variables that hold secrets in the pod: only that they are set
# matters, so each is given a dummy value.
export def run_checks [checks: list, node_registry: string, images: list] {
    if ($checks | is-empty) { return [] }
    let located = ($checks | get image | uniq | each {|i| locate $images $node_registry $i })
    with_scratch_namespace {|ns|
        let objects = ($checks | each {|c|
            let l = ($located | where image == $c.image | first)
            let name = $"check-($c.workload)"
            [
                (document_config_map $ns $name $c.document)
                (agent_pod $ns $name $l.node ((image_command $l) ++ ["--check" "--config" $DOCUMENT_PATH]) $name ($c.secrets | each {|s| { name: $s, value: "x" } }))
            ]
        } | flatten)
        run_pods $ns $objects | each {|r| check_problems ($r.name | str replace --regex '^check-' '') $r.exit $r.logs } | flatten
    }
}
