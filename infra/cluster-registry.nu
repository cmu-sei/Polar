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

# infra/cluster-registry.nu
#
# A registry in the target's own cluster, for a target whose cluster settings say `registry_in_cluster: { node_port: N }`: then
# no registry, and no container runtime to run one, is needed on this machine. The nodes pull from it at `localhost:N`
# (infra/registry.nu): a NodePort answers on every node's localhost, and a node's container runtime pulls from a localhost
# registry over plain HTTP with no configuration (checked on u7s, containerd 2.2). This machine pushes to it through a
# port-forward (`just port-forwards-up registry`, to localhost:5000 or POLAR_REGISTRY_FORWARD_PORT).
#
# It has to exist before anything that pulls a Polar image, the render's scaffold pods and Flux's own controllers included, so
# up.nu applies it itself, first, and it is not in any layer. Its images, and the storage that holds them, come from their
# upstream registries, pinned by digest: they cannot come from a registry that does not exist yet.
#
# Not yet: authentication. Anyone who can reach a node's address at the NodePort can push and pull.
#
#   nu infra/cluster-registry.nu up <target>        apply it (and the target's storage, if it deploys it), wait, start the forward
#   nu infra/cluster-registry.nu status <target>

use registry.nu [target_cluster]
use retry.nu [with_retries]
use dhall-cache.nu

export const NAMESPACE = "polar-registry"
export const NAME = "registry"

# The registry, and what the storage chart runs, from upstream by digest (each an index with linux/amd64 and linux/arm64).
export const REGISTRY_IMAGE = "docker.io/library/registry:2.8.3@sha256:a3d8aaa63ed8681a604f1dea0aa03f100d5895b6a58ace528858a7b332415373"
export const UPSTREAM_STORAGE_IMAGES = {
    "node-registry.invalid/local-path-provisioner:v0.0.35": "docker.io/rancher/local-path-provisioner:v0.0.35@sha256:34ff0847cc47ebf69656ba44a3de9324596d0036b66ffd323b21614dd8221530"
    "node-registry.invalid/busybox:latest": "docker.io/library/busybox:1.37.0@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e"
}

const INFRA = (path self | path dirname)

# ── Settings and manifests (pure) ────────────────────────────────────────────

# The in-cluster registry's settings for a target's merged cluster settings (`cluster.nuon` under the target's own), or null when
# the target does not keep its registry in the cluster.
export def settings_of [cluster: record] {
    let rc = ($cluster.registry_in_cluster? | default null)
    if $rc == null { return null }
    let port = $rc.node_port
    if ($port | describe) != "int" or $port < 30000 or $port > 32767 {
        error make --unspanned { msg: $"registry_in_cluster.node_port is ($port): a NodePort is between 30000 and 32767" }
    }
    {
        node_port: $port
        local_port: ($rc.local_port? | default 5000)
        size: ($rc.size? | default "20Gi")
        storage_class: $cluster.storage.class
    }
}

export def manifests [s: record] {
    let labels = { app: $NAME }
    [
        { apiVersion: "v1", kind: "Namespace", metadata: { name: $NAMESPACE } }
        {
            apiVersion: "v1", kind: "PersistentVolumeClaim"
            metadata: { name: $NAME, namespace: $NAMESPACE }
            spec: { accessModes: ["ReadWriteOnce"], storageClassName: $s.storage_class, resources: { requests: { storage: $s.size } } }
        }
        {
            apiVersion: "apps/v1", kind: "Deployment"
            metadata: { name: $NAME, namespace: $NAMESPACE, labels: $labels }
            spec: {
                replicas: 1
                # One pod at a time: the volume is ReadWriteOnce.
                strategy: { type: "Recreate" }
                selector: { matchLabels: $labels }
                template: {
                    metadata: { labels: $labels }
                    spec: {
                        automountServiceAccountToken: false
                        containers: [{
                            name: $NAME
                            image: $REGISTRY_IMAGE
                            ports: [{ containerPort: 5000 }]
                            env: [{ name: "REGISTRY_STORAGE_DELETE_ENABLED", value: "true" }]
                            readinessProbe: { httpGet: { path: "/v2/", port: 5000 }, periodSeconds: 5 }
                            volumeMounts: [{ name: "data", mountPath: "/var/lib/registry" }]
                        }]
                        volumes: [{ name: "data", persistentVolumeClaim: { claimName: $NAME } }]
                    }
                }
            }
        }
        {
            apiVersion: "v1", kind: "Service"
            metadata: { name: $NAME, namespace: $NAMESPACE }
            spec: { type: "NodePort", selector: $labels, ports: [{ port: 5000, targetPort: 5000, nodePort: $s.node_port }] }
        }
    ]
}

# The storage chart's local-path-provisioner manifest with its images put in: from the node registry, or, for a target whose
# registry is in the cluster, from upstream by digest, since the registry's own volume comes from this provisioner.
export def storage_images [text: string, node_registry: string, in_cluster: bool] {
    if $in_cluster {
        $UPSTREAM_STORAGE_IMAGES | transpose from to | reduce --fold $text {|it, acc| $acc | str replace --all $it.from $it.to }
    } else {
        $text | str replace --all "node-registry.invalid/" $"($node_registry)/"
    }
}

# ── The cluster ──────────────────────────────────────────────────────────────

# The target's merged cluster settings.
export def cluster_of [target: string] {
    open ($INFRA | path join "config" "cluster.nuon") | merge deep (target_cluster $target)
}

def kubectl_apply [objects: list] {
    let r = ({ apiVersion: "v1", kind: "List", items: $objects } | to json | ^kubectl apply -f - | complete)
    if $r.exit_code != 0 { error make --unspanned { msg: $"the cluster refused the registry's objects: ($r.stderr | str trim)" } }
}

# The target's storage, when it deploys the storage chart and the cluster does not have its storage class yet: rendered and applied
# now, because the registry's volume needs it and the render that would otherwise make it needs the registry. The later apply of
# the same manifests changes nothing. A class that is already there is left as it is: whatever provides it is not this run's.
def ensure_storage [target: string, storage_class: string] {
    let config = (^nu ($INFRA | path join "targets" $target "target.nu") | from nuon)
    let chart = ($config.charts | where {|c| ($c | path dirname | path basename) == "storage" })
    if ($chart | is-empty) { print "  -- the target does not deploy the storage chart: its cluster provides the storage class"; return }
    if (do { ^kubectl get storageclass $storage_class } | complete).exit_code == 0 {
        print $"  -- storage class ($storage_class) is already there: leaving it as it is"
        return
    }
    # Rendered before render.nu runs, so Dhall's cache is filled here first.
    dhall-cache seed | ignore
    let out = (mktemp --directory)
    let context = { chart_dir: ($chart | first | path dirname), output_dir: $out, overrides: $config.overrides, repo_root: $config.repo_root, target_dir: $config.target_dir, enable_tls: $config.enable_tls }
    let r = (do { ^nu ($chart | first) ($context | to nuon) } | complete)
    if $r.exit_code != 0 { rm -rf $out; error make --unspanned { msg: $"rendering the storage chart failed: ($r.stderr | str trim)" } }
    let applied = (do { ^kubectl apply -f ($out | path join "local-path-provisioner.yaml") -f ($out | path join "storage-class.yaml") } | complete)
    rm -rf $out
    if $applied.exit_code != 0 { error make --unspanned { msg: $"applying the storage chart failed: ($applied.stderr | str trim)" } }
    print "  ok: storage applied (local-path-provisioner from upstream)"
}

# Whether the registry answers at this machine's end of the forward.
export def answers [local_port: int] {
    let r = (do { ^curl -s -m 3 -o /dev/null -w "%{http_code}" $"http://localhost:($local_port)/v2/" } | complete)
    $r.exit_code == 0 and $r.stdout == "200"
}

def forward_running [] {
    (do { ^pgrep -f $"port-forward -n ($NAMESPACE) svc/($NAME)" } | complete).exit_code == 0
}

# Applies the registry for `target` (and its storage first), waits until it is ready, and starts the forward this machine pushes
# through. Returns the local address to push to, for POLAR_REGISTRY. Idempotent.
export def ensure [target: string] {
    let s = (settings_of (cluster_of $target))
    if $s == null { error make --unspanned { msg: $"target ($target) does not keep its registry in the cluster \(no cluster.registry_in_cluster\)" } }
    ensure_storage $target $s.storage_class
    kubectl_apply (manifests $s)
    let rolled = (do { ^kubectl -n $NAMESPACE rollout status $"deployment/($NAME)" --timeout=5m } | complete)
    if $rolled.exit_code != 0 { error make --unspanned { msg: $"the registry did not become ready: ($rolled.stderr | str trim) \(kubectl -n ($NAMESPACE) describe pod -l app=($NAME)\)" } }
    print $"  ok: the registry runs in the cluster; the nodes pull from localhost:($s.node_port)"
    if (forward_running) and not (answers $s.local_port) {
        # A forward left from before (to a cluster since rebuilt, or a pod since replaced) holds the port and answers nothing: replace it.
        print "  -- the registry's port-forward does not answer: restarting it"
        do { ^pkill -f $"port-forward -n ($NAMESPACE) svc/($NAME)" } | complete | ignore
        sleep 1sec
    }
    if not (forward_running) {
        if (answers $s.local_port) {
            error make --unspanned { msg: $"something else already answers as a registry at localhost:($s.local_port) \(the host registry? just registry-down\), or set cluster.registry_in_cluster.local_port" }
        }
        let started = (with-env { POLAR_REGISTRY_FORWARD_PORT: ($s.local_port | into string) } { do { ^just port-forwards-up registry } | complete })
        if $started.exit_code != 0 { error make --unspanned { msg: $"starting the registry's port-forward failed: ($started.stderr | str trim)" } }
    }
    let up = (with_retries 20 1sec {|n| { ok: (answers $s.local_port), final: false, detail: $"nothing answers at localhost:($s.local_port)" } })
    if not $up.ok { error make --unspanned { msg: $"the registry's port-forward does not answer at localhost:($s.local_port) \(/tmp/pf-registry.log\)" } }
    print $"  ok: this machine pushes to it at localhost:($s.local_port)"
    $"localhost:($s.local_port)"
}

def "main up" [target: string] {
    let at = (ensure $target)
    print $"push with POLAR_REGISTRY=($at), e.g. nu infra/images.nu import --from docker.io/<account>"
}

def "main status" [target: string] {
    let s = (settings_of (cluster_of $target))
    if $s == null { print $"($target) does not keep its registry in the cluster"; return }
    print $"nodes pull from: localhost:($s.node_port)"
    print $"forward: (if (forward_running) { 'running' } else { 'not running' }), registry (if (answers $s.local_port) { 'answers' } else { 'does not answer' }) at localhost:($s.local_port)"
}

def main [] { help main }
