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

# infra/deploy-objects.nu
#
# What goes into a target's deployment repository besides the rendered manifests: the Flux objects that
# reconcile them, and the record of where they came from. All of it is decided here, from the target's layers and
# the rendered files, so it can be tested without a cluster or a git server. `deploy-repo.nu publish` writes it.
#
# The repository: layers/<layer>/<files>, flux/ (the GitRepository and one Kustomization per layer),
# provenance.json, README.md. The layers a cluster's installer applies itself (BOOTSTRAP_LAYERS: Flux's own
# controllers) are not in it: Flux cannot reconcile itself into existence.

use apply.nu [config_users]

# Layers the installer applies before Flux exists.
export const BOOTSTRAP_LAYERS = ["flux"]
export const SOURCE_NAME = "polar-deploy"
export const FLUX_NS = "flux-system"
export const AUTH_SECRET = "deploy-repo-auth"

# The layers Flux reconciles. A dependency on a bootstrap layer becomes that layer's own dependencies, so the
# first published layer depends on whatever the installer applied.
export def published_layers [layers: list] {
    let skipped = ($layers | where name in $BOOTSTRAP_LAYERS)
    let skipped_names = ($skipped | get name)
    $layers
    | where name not-in $skipped_names
    | each {|l|
        let deps = ($l.depends_on | each {|d|
            if $d in $skipped_names { $skipped | where name == $d | first | get depends_on } else { [$d] }
        } | flatten | uniq)
        $l | update depends_on $deps
    }
}

# The YAML documents of a rendered file, each parsed on its own (`from yaml` merges a multi-document file).
export def documents [text: string] {
    $text
    | split row --regex '(?m)^---\s*$'
    | where {|d| ($d | lines | where {|l| ($l | str trim) != "" and not ($l | str trim | str starts-with "#") } | is-not-empty) }
    | each {|d| $d | from yaml }
}

# Whether a rendered file holds a Job (their pod templates are immutable, so the layer must be forced).
export def has_job [text: string] {
    (documents $text | where {|d| ($d.kind? | default "") == "Job" } | is-not-empty)
}

# Image references in rendered text that are not pinned by digest. `image:` lines only.
export def unpinned_images [text: string] {
    $text
    | lines
    | where {|l| $l =~ '^\s*-?\s*["'']?image["'']?:\s' }
    | each {|l| $l | str replace --regex '^\s*-?\s*["'']?image["'']?:\s*' '' | str trim --char '"' | str trim --char "'" }
    | where {|i| $i != "" and not ($i | str contains "@sha256:") }
    | uniq
}

# The workload a `wait_for` entry (as `up.nu` waits for it) is, among the documents of its layer: a Flux health check.
export def health_check [wait: record, docs: list] {
    let ns = $wait.ns
    if $wait.kind == "statefulset" {
        let name = ($wait.ref | str replace "statefulset/" "")
        return { apiVersion: "apps/v1", kind: "StatefulSet", name: $name, namespace: $ns }
    }
    let parts = ($wait.ref | split row "=")
    let key = ($parts | first)
    let value = ($parts | skip 1 | str join "=")
    if $key == "job-name" {
        let found = ($docs | where {|d| ($d.kind? | default "") == "Job" and ($d.metadata?.name? | default "") == $value })
        if ($found | is-empty) { error make --unspanned { msg: $"the layer waits for the pods of Job ($value) and has no such Job" } }
        # Flux's own Job reader (it runs before any expression) counts a Job as healthy only when it has completed, and an
        # agent never completes. What says the agent is up is the Service that selects its pods: its Endpoints lists only
        # ready pods, and has a name Flux can be given. A Job with no such Service has nothing to wait for.
        let labels = ($found | first | get -o spec.template.metadata.labels | default {})
        let svc = ($docs | where {|d|
            let selector = ($d | get -o spec.selector | default {})
            let in_ns = (($d | get -o metadata.namespace | default $ns) == $ns)
            (($d.kind? | default "") == "Service") and $in_ns and ($selector | is-not-empty) and ($selector | transpose k v | all {|kv| ($labels | get -o $kv.k) == $kv.v })
        })
        if ($svc | is-empty) { return null }
        return { apiVersion: "v1", kind: "Endpoints", name: ($svc | first | get metadata.name), namespace: $ns }
    }
    let found = ($docs | where {|d|
        (($d.kind? | default "") in ["Deployment" "StatefulSet" "DaemonSet"]) and (($d.spec?.template?.metadata?.labels? | default {} | get -o $key) == $value)
    })
    if ($found | is-empty) { error make --unspanned { msg: $"the layer waits for pods labelled ($wait.ref) and no Deployment, StatefulSet or DaemonSet in it makes them" } }
    let w = ($found | first)
    { apiVersion: $w.apiVersion, kind: $w.kind, name: $w.metadata.name, namespace: ($w.metadata.namespace? | default $ns) }
}

# An Endpoints object has no `subsets` while no pod is ready (a missing field makes Flux keep waiting), and lists the ready
# pods' addresses once there are some.
export def endpoints_health_expr [] {
    { apiVersion: "v1", kind: "Endpoints", current: "subsets.exists(s, has(s.addresses) && size(s.addresses) > 0)" }
}

# The Kustomization for one layer. `docs` are the documents of all its files.
export def kustomization [layer: record, docs: list, forced: bool] {
    let checks = ($layer.wait_for | each {|w| health_check $w $docs } | compact)
    let base = {
        apiVersion: "kustomize.toolkit.fluxcd.io/v1"
        kind: "Kustomization"
        metadata: { name: $"polar-($layer.name)", namespace: $FLUX_NS }
        spec: {
            interval: "10m"
            timeout: "10m"
            retryInterval: "1m"
            path: $"./layers/($layer.name)"
            prune: true
            sourceRef: { kind: "GitRepository", name: $SOURCE_NAME }
        }
    }
    let with_deps = if ($layer.depends_on | is-empty) { $base } else {
        $base | update spec ($base.spec | insert dependsOn ($layer.depends_on | each {|d| { name: $"polar-($d)" } }))
    }
    let with_force = if $forced { $with_deps | update spec ($with_deps.spec | insert force true) } else { $with_deps }
    if ($checks | is-empty) { return $with_force }
    let with_checks = ($with_force | update spec ($with_force.spec | insert healthChecks $checks))
    if ($checks | any {|c| $c.kind == "Endpoints" }) {
        $with_checks | update spec ($with_checks.spec | insert healthCheckExprs [(endpoints_health_expr)])
    } else { $with_checks }
}

# The GitRepository Flux reads. `host` is the name on the server's certificate (a Service in the cluster whose
# Endpoints are the host), `secret` holds the `flux` identity's tls.crt, tls.key and ca.crt.
export def git_repository [target: string, host: string, port: int] {
    {
        apiVersion: "source.toolkit.fluxcd.io/v1"
        kind: "GitRepository"
        metadata: { name: $SOURCE_NAME, namespace: $FLUX_NS }
        spec: {
            interval: "1m"
            url: $"https://($host):($port)/deploy-($target).git"
            ref: { branch: "main" }
            secretRef: { name: $AUTH_SECRET }
        }
    }
}

# YAML documents joined into one file.
export def yaml_docs [docs: list] {
    $docs | each {|d| $d | to yaml | str trim --right } | str join "\n---\n" | $in + "\n"
}

export const CONFIG_HASH_ANNOTATION = "polar.dev/config-hash"

# A fingerprint of a ConfigMap's content.
def configmap_hash [doc: record] {
    { data: ($doc | get -o data | default {}), binaryData: ($doc | get -o binaryData | default {}) } | to json --raw | hash sha256
}

# The rendered files with, in every Job, Deployment and StatefulSet that uses a ConfigMap, an annotation in its pod
# template that is a fingerprint of those ConfigMaps. Flux recreates a Job only when its spec changes, and a changed
# ConfigMap does not change it: with the fingerprint, it does, and the workload restarts to read the new content
# (`apply.nu` does the same for the imperative path). A file with no such workload is returned as it was rendered.
export def annotate_config_hashes [texts: record] {
    let maps = ($texts | items {|f, t| documents $t | where {|d| ($d.kind? | default "") == "ConfigMap" } }
        | flatten
        | reduce --fold {} {|d, acc| $acc | upsert $"($d.metadata.namespace? | default 'polar')/($d.metadata.name)" (configmap_hash $d) })
    $texts | items {|f, text|
        let users = (config_users $text | each {|u|
            let known = ($u.configmaps | where {|c| $"($u.ns)/($c)" in ($maps | columns) })
            if ($known | is-empty) { null } else {
                { kind: $u.kind, name: $u.name, hash: ($known | sort | each {|c| $"($c)=($maps | get $"($u.ns)/($c)")" } | str join "\n" | hash sha256) }
            }
        } | compact)
        if ($users | is-empty) { return { f: $f, text: $text } }
        let docs = (documents $text | each {|d|
            let hit = ($users | where kind == ($d.kind? | default "") and name == ($d.metadata?.name? | default ""))
            if ($hit | is-empty) { $d } else {
                let annotations = ($d | get -o spec.template.metadata.annotations | default {} | upsert $CONFIG_HASH_ANNOTATION ($hit | first | get hash))
                $d | upsert spec.template.metadata.annotations $annotations
            }
        })
        { f: $f, text: (yaml_docs $docs) }
    } | reduce --fold {} {|r, acc| $acc | insert $r.f $r.text }
}

# What the published tree records about where it came from. Only facts that exist before the cluster does: nothing
# here is read from a cluster, and the signature's trust is the deployment CA's, not Polar's.
export def provenance [facts: record] {
    $facts | to json --indent 2
}

export def readme [target: string] {
    $"# deploy-($target)\n\nGenerated by `nu infra/deploy-repo.nu publish ($target)`. Do not edit: the next publish replaces everything here.\n\n- `layers/<name>/` the rendered manifests of one layer, as Flux reconciles them.\n- `flux/` the GitRepository and the Kustomizations \(one per layer, in dependency order\) that `flux.nu install` applies.\n- `provenance.json` where this tree came from: the source commit, the digest of every file, the image lock, the CAs.\n\nEvery commit is signed with the pipeline's certificate, which chains to the deployment CA \(not Polar's\).\n"
}
