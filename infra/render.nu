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

# infra/render.nu
#
# Polar infrastructure rendering pipeline.
# Reads target configuration, validates environment, renders all charts.
#
# Usage:
#   nu infra/render.nu <target>
#   nu infra/render.nu local-minimal
#
# Output goes to the directory specified in target.nu's output_dir. This script
# only renders; it never touches a cluster. To apply the result and bring a
# cluster up, run:
#   nu infra/up.nu <target>

use validate.nu *
use rules.nu [job_replacement_problems, apply_order_problems, pull_policy_problems]
use layers.nu [layer_problems]
use registry.nu [node_registry]
use lock.nu [effective, lock_problems, pin_text]
use dhall-cache.nu

def main [
    target: string        # Target name: local, local-minimal, sandbox-local
    --charts-into: string = ""  # render only the charts, into this directory, and stop: what their manifests name (the images to
                                # build before the render) needs no agent's scaffold. The target's output is left alone.
] {
    let infra_root  = ($env.PWD | path join "infra")
    let target_file = ($infra_root | path join $"targets/($target)/target.nu")

    if not ($target_file | path exists) {
        print $"ERROR: Unknown target '($target)'"
        print $"  No target.nu found at ($target_file)"
        exit 1
    }

    print $"=== Polar Render Pipeline ==="
    print $"  target: ($target)"
    # The registry as the nodes name it, resolved once (infra/registry.nu) and handed to everything that renders: the charts' Dhall reads it
    # from the environment, and so do the processes that run them.
    $env.POLAR_NODE_REGISTRY = (node_registry $target)
    print $"  registry: ($env.POLAR_NODE_REGISTRY)"
    let seeded = (dhall-cache seed)
    if $seeded > 0 { print $"  dhall: ($seeded) pinned import\(s\) copied into its cache, so nothing is fetched" }
    print ""

    # ── Load target configuration ─────────────────────────────────────────────
    print "Loading target configuration..."
    let config = (nu $target_file | from nuon)
    let output_dir = if ($charts_into | is-empty) { $config.output_dir } else { $charts_into }

    # ── Validate environment ───────────────────────────────────────────────────
    print "Validating environment..."
    validate_env $config.required_env


    print ""

    # ── Prepare output directory ───────────────────────────────────────────────
    if ($output_dir | path exists) {
        rm -rf $output_dir
    }
    mkdir $output_dir

    # ── Build context record passed to every chart main.nu ────────────────────
    # Each chart gets this base context, extended with chart-specific fields.
    let base_context = {
        output_dir  : $output_dir
        overrides   : $config.overrides
        repo_root   : $config.repo_root
        target_dir  : $config.target_dir
        enable_tls  : $config.enable_tls
    }

    # Cross-service resolved values injected into dependent charts
    let neo4j_bolt_addr      = $config.neo4j_bolt_addr
    let cert_issuer_url      = $config.cert_issuer_url
    let cert_issuer_audience = $config.cert_issuer_audience
    let cert_issuer_ca_configmap = ($config.cert_issuer_ca_configmap? | default "")
    let polar_init_image      = $config.polar_init_image
    # Where a workload that is not a bootstrap identity asks for its certificate: the credentials agent's
    # observer, from services.nuon. The hand-written charts below that are not bootstrap identities get it.
    let credentials_observer_url = (^nu ($infra_root | path join "resolve.nu") service-url credentials-agent-observer --target $target | str trim)

    let identity_lifetime_overrides = $config.identity_lifetime_overrides

    # ── Render each chart ─────────────────────────────────────────────────────
    print "Rendering charts..."
    print ""

    for chart_path in $config.charts {
        let chart_dir  = ($chart_path | path dirname)
        let chart_name = ($chart_dir | path basename)

        print $"── ($chart_name) ──"

        # Build chart-specific context by extending base context
        let chart_context = ($base_context | merge { chart_dir: $chart_dir } | merge (
            match $chart_name {
                "cert-issuer" => {
                    identityLifetimeOverrides: $identity_lifetime_overrides
                    # The floor is written from the catalog, so what a workload is issued and what cert-issuer
                    # allows it cannot differ. The catalog has it from the agents' scaffolds, which a charts-only render
                    # (for the images its manifests name) does not have: it gives none, since the floor names no image.
                    floor: (if ($charts_into | is-not-empty) { [] } else { ^nu ($infra_root | path join "resolve.nu") floor --target $target | from json })
                    floorEnforced: (^nu ($infra_root | path join "resolve.nu") floor-enforced --target $target | str trim | into bool)
                }
                "neo4j" => { polarInitImage: $polar_init_image, credentialsObserverUrl: $credentials_observer_url }
                "gitlab" => { neo4jBoltAddr: $neo4j_bolt_addr, certIssuerUrl: $cert_issuer_url, certIssuerAudience: $cert_issuer_audience, polarInitImage: $polar_init_image }
                "git"         => { neo4jBoltAddr: $neo4j_bolt_addr, certIssuerUrl: $cert_issuer_url, certIssuerAudience: $cert_issuer_audience, polarInitImage: $polar_init_image }
                "build-processor" => { neo4jBoltAddr: $neo4j_bolt_addr, certIssuerUrl: $cert_issuer_url, certIssuerAudience: $cert_issuer_audience, polarInitImage: $polar_init_image }
                "openapi"     => { neo4jBoltAddr: $neo4j_bolt_addr, certIssuerUrl: $cert_issuer_url, certIssuerAudience: $cert_issuer_audience, polarInitImage: $polar_init_image }
                "ace"         => { neo4jBoltAddr: $neo4j_bolt_addr, certIssuerUrl: $cert_issuer_url, certIssuerAudience: $cert_issuer_audience, polarInitImage: $polar_init_image, credentialsObserverUrl: $credentials_observer_url }
                _             => { _placeholder: "" }
            }
        ))

        nu $chart_path ($chart_context | to nuon)
        print ""
    }
    if ($charts_into | is-not-empty) {
        print $"=== Charts rendered into ($charts_into) ==="
        return
    }

    print "Rendering catalog workloads..."
    ^nu ($infra_root | path join "render-catalog.nu") $target $output_dir
    if $env.LAST_EXIT_CODE != 0 { exit 1 }
    print ""
    print "Checking the apply order covers the catalog's manifests..."
    let catalog_names = (^nu ($infra_root | path join "resolve.nu") names --target $target | from json)
    let catalog_files = ($catalog_names | each {|n| [$"($n).yaml" $"($n)-resources.yaml"] } | flatten
        | where {|f| ($output_dir | path join $f | path exists) })
    let order_problems = (apply_order_problems $catalog_files $config.apply_order)
    if ($order_problems | is-not-empty) {
        for p in $order_problems { print -e $"  ERROR: ($p)" }
        exit 1
    }
    print ""
    print "Checking workload ServiceAccounts..."
    check_and_emit_service_accounts $output_dir
    print ""

    print "Checking Job pod replacement policy..."
    check_job_replacement_policy $output_dir
    print ""

    print "Checking image pull policies..."
    check_pull_policies $output_dir
    print ""

    print "Checking cert-issuer's TLS identity..."
    check_cert_issuer_identity $output_dir $cert_issuer_url
    print ""

    print "Checking CA provisioning consistency..."
    check_ca_provisioning $output_dir $cert_issuer_ca_configmap
    print ""
    # A target that deploys by digest names every image of the node registry by digest, from the image lock. An
    # image the lock does not know is an error, never left on a tag.
    if ($config.pin_images? | default false) {
        print "Pinning images by digest..."
        let cluster = (open ($infra_root | path join "config" "cluster.nuon"))
        let rows = (effective)
        let lock_issues = (lock_problems $rows)
        if ($lock_issues | is-not-empty) {
            for p in $lock_issues { print -e $"  ERROR: ($p)" }
            exit 1
        }
        mut missing = []
        mut pinned = 0
        for f in (glob ($output_dir | path join "*.yaml")) {
            let text = (open --raw $f)
            let r = (pin_text $text $rows $env.POLAR_NODE_REGISTRY $cluster.arch)
            if $r.text != $text { $r.text | save --force $f }
            $missing = ($missing | append $r.missing)
            $pinned = $pinned + $r.pinned
        }
        let unknown = ($missing | uniq | sort)
        if ($unknown | is-not-empty) {
            print -e $"  ERROR: the image lock has no ($cluster.arch) digest for: ($unknown | str join ', ')"
            print -e "  Push them (nu infra/images.nu push NAME) or record what the registry holds (nu infra/images.nu lock refresh)."
            exit 1
        }
        print $"  ok: ($pinned) image references pinned"
        print ""
    }

    # Last, because the checks above add files (serviceaccounts.yaml) that the layers must hold too.
    if ($config.apply_layers? | default [] | is-not-empty) {
        print "Checking the layers hold everything rendered..."
        let rendered = (glob ($output_dir | path join "*.yaml") | each {|f| $f | path basename } | sort)
        let layer_issues = (layer_problems $config.apply_layers $rendered)
        if ($layer_issues | is-not-empty) {
            for p in $layer_issues { print -e $"  ERROR: ($p)" }
            exit 1
        }
        print $"  ok: ($config.apply_layers | length) layers hold the ($rendered | length) files"
        print ""
    }

    # The target-level part of the topic cross-check: a declared topic one-sided here is a warning (the catalog-wide
    # check, which fails, is infra/tests/catalog-topics.nu).
    if ($charts_into | is-empty) {
        ^nu ($infra_root | path join "resolve.nu") topics-check --target $target
        print ""
    }

    print "=== Render complete ==="
    print $"  output: ($output_dir)"
    print ""

    # ── Next step ─────────────────────────────────────────────────────────────
    # Rendering never touches the cluster. `infra/up.nu` applies the result and
    # walks the rest of the bring-up (CA, secrets, admin flow).
    print "To bring up a cluster from this render:"
    print $"  nu infra/up.nu ($target)"
    print ""
}

# Splits one rendered file into YAML documents. Anything unparseable is
# reported, not silently dropped, since a skipped workload would let the
# check below pass wrongly.
def docs_in_file [file: string] {
    open --raw $file
    | split row --regex '(?m)^---\s*$'
    | where {|d| ($d | str trim | str length) > 0 }
    | each {|d|
        try { $d | from yaml } catch { print $"  WARN: skipped an unparseable document in ($file)"; null }
    }
    | where {|d| ($d | describe | str starts-with "record") }
}

# A workload's ServiceAccount name becomes the SAN in its certificate
# (<sa>.<namespace>.serviceaccount.cluster.local), so two workloads sharing
# one, or falling back to `default`, are indistinguishable at the
# application layer. Fails the render on either, then emits a ServiceAccount
# for every workload identity no rendered manifest already declares.
def check_job_replacement_policy [output_dir: string] {
    let files = (ls $output_dir | get name | where {|f| $f | str ends-with ".yaml" })
    let docs = ($files | reduce --fold [] {|f, acc| $acc ++ (docs_in_file $f) })
    let problems = (job_replacement_problems $docs)
    for p in $problems { print $"  ERROR: ($p)" }
    if ($problems | length) > 0 { exit 1 }
}

# A tag that moves must be pulled Always (infra/rules.nu pull_policy_problems). The check runs on what is about to be applied, wherever
# a chart got its policy from, so a new chart cannot bring the problem back.
def check_pull_policies [output_dir: string] {
    let files = (ls $output_dir | get name | where {|f| $f | str ends-with ".yaml" })
    let docs = ($files | reduce --fold [] {|f, acc| $acc ++ (docs_in_file $f) })
    let problems = (pull_policy_problems $docs)
    for p in $problems { print $"  ERROR: ($p)" }
    if ($problems | length) > 0 { exit 1 }
}

def check_and_emit_service_accounts [output_dir: string] {
    let files = (ls $output_dir | get name | where {|f| $f | str ends-with ".yaml" })
    let docs = ($files | reduce --fold [] {|f, acc| $acc ++ (docs_in_file $f) })

    let workloads = (
        $docs
        | where {|d| ($d.kind? | default "") in ["Job" "Deployment" "StatefulSet" "DaemonSet"] }
        | each {|d|
            let pod = $d.spec.template.spec
            {
                kind: $d.kind
                name: $d.metadata.name
                namespace: ($d.metadata.namespace? | default "default")
                sa: ($pod.serviceAccountName? | default "default")
                automount: ($pod.automountServiceAccountToken? | default false)
            }
        }
        | each {|w| $w | insert identity $"($w.sa).($w.namespace)" }
    )

    let defaulted = ($workloads | where sa == "default")
    let dupes = (
        $workloads
        | where sa != "default"
        | group-by identity
        | transpose identity items
        | where {|g| ($g.items | length) > 1 }
    )

    for w in $defaulted {
        print $"  ERROR: ($w.kind) ($w.namespace)/($w.name) runs as the default ServiceAccount"
    }
    for g in $dupes {
        let who = ($g.items | each {|w| $"($w.kind) ($w.name)" } | str join ", ")
        print $"  ERROR: identity ($g.identity) is shared by: ($who)"
    }
    if ($defaulted | length) > 0 or ($dupes | length) > 0 { exit 1 }

    let declared = (
        $docs
        | where {|d| ($d.kind? | default "") == "ServiceAccount" }
        | each {|d| let ns = ($d.metadata.namespace? | default "default"); $"($d.metadata.name).($ns)" }
    )
    let missing = ($workloads | where {|w| not ($w.identity in $declared) })

    if ($missing | length) > 0 {
        $missing
        | each {|w|
            let sa = {
                apiVersion: "v1"
                kind: "ServiceAccount"
                metadata: { name: $w.sa, namespace: $w.namespace }
                automountServiceAccountToken: $w.automount
            }
            "---\n" + ($sa | to yaml) + "\n"
        }
        | str join ""
        | save --force ($output_dir | path join "serviceaccounts.yaml")
        print $"  emitted ($missing | length) ServiceAccount objects"
    }
}

# cert-issuer's TLS certificate carries mtls.server_identity as its SAN, and
# every caller verifies the server against the host in its URL. If the two
# differ, every caller fails the handshake with an error that looks like a
# network problem, so catch it here, at render time.
def check_cert_issuer_identity [output_dir: string, cert_issuer_url: string] {
    let cfg_file = ($output_dir | path join "cert-issuer-configmap.yaml")
    if not ($cfg_file | path exists) { return }

    let server_identity = (
        open $cfg_file
        | get data."cert-issuer.json"
        | from json
        | get mtls.server_identity
    )
    let url_host = ($cert_issuer_url | url parse | get host)

    if $server_identity != $url_host {
        print $"  ERROR: cert-issuer's TLS identity is ($server_identity) but callers connect to ($url_host)"
        exit 1
    }
}

# Callers mount the Polar CA ConfigMap only when one is configured, and a pod
# that mounts a ConfigMap which doesn't exist never starts. That ConfigMap is
# created by bootstrap-ca, and cert-issuer only loads a provisioned CA. So a
# configured ConfigMap with cert-issuer still self-bootstrapping is the
# failure to catch here. The opposite (cert-issuer provisioned, callers not
# mounting) is harmless: callers fetch and pin.
def check_ca_provisioning [output_dir: string, ca_configmap: string] {
    let cfg_file = ($output_dir | path join "cert-issuer-configmap.yaml")
    if not ($cfg_file | path exists) { return }

    let provisioning = (
        open $cfg_file
        | get data."cert-issuer.json"
        | from json
        | get ca.provisioning
    )

    if ($ca_configmap | is-not-empty) and $provisioning != "provisioned" {
        print $"  ERROR: callers mount the ConfigMap '($ca_configmap)', but cert-issuer's ca.provisioning is '($provisioning)'."
        print "  That ConfigMap only exists once bootstrap-ca has run, and cert-issuer must load that CA (Provisioned) for callers to verify against it."
        exit 1
    }
}
