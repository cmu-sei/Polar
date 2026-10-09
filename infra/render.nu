#!/usr/bin/env nu
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
use rules.nu [job_replacement_problems, apply_order_problems]

def main [
    target: string        # Target name: local, sandbox, ace
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
    print ""

    # ── Load target configuration ─────────────────────────────────────────────
    print "Loading target configuration..."
    let config = (nu $target_file | from nuon)
    let output_dir = $config.output_dir

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
    let jaeger_dns_name      = $config.jaeger_dns_name
    let scheduler_remote_url = $config.scheduler_remote_url
    let cert_issuer_url      = $config.cert_issuer_url
    let cert_issuer_audience = $config.cert_issuer_audience
    let cert_issuer_ca_configmap = ($config.cert_issuer_ca_configmap? | default "")
    let polar_init_image      = $config.polar_init_image

    let cassini_shutdown_token = $config.cassini_shutdown_token
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
                "cert-issuer" => { identityLifetimeOverrides: $identity_lifetime_overrides }
                "neo4j" => { polarInitImage: $polar_init_image }
                "cassini" => { jaegerDNSName: $jaeger_dns_name, certIssuerUrl: $cert_issuer_url, certIssuerAudience: $cert_issuer_audience, polarInitImage: $polar_init_image, cassiniShutdownToken: $cassini_shutdown_token }
                "gitlab" => { neo4jBoltAddr: $neo4j_bolt_addr, certIssuerUrl: $cert_issuer_url, certIssuerAudience: $cert_issuer_audience, polarInitImage: $polar_init_image }
                "git"         => { neo4jBoltAddr: $neo4j_bolt_addr, certIssuerUrl: $cert_issuer_url, certIssuerAudience: $cert_issuer_audience, polarInitImage: $polar_init_image }
                "build-processor" => { neo4jBoltAddr: $neo4j_bolt_addr, certIssuerUrl: $cert_issuer_url, certIssuerAudience: $cert_issuer_audience, polarInitImage: $polar_init_image }
                "resolver"        => { certIssuerUrl: $cert_issuer_url, certIssuerAudience: $cert_issuer_audience, polarInitImage: $polar_init_image }
                "scheduler"   => { neo4jBoltAddr: $neo4j_bolt_addr, remoteUrl: $scheduler_remote_url, certIssuerUrl: $cert_issuer_url, certIssuerAudience: $cert_issuer_audience, polarInitImage: $polar_init_image }
                "openapi"     => { neo4jBoltAddr: $neo4j_bolt_addr, certIssuerUrl: $cert_issuer_url, certIssuerAudience: $cert_issuer_audience, polarInitImage: $polar_init_image }
                "ace"         => { neo4jBoltAddr: $neo4j_bolt_addr, certIssuerUrl: $cert_issuer_url, certIssuerAudience: $cert_issuer_audience, polarInitImage: $polar_init_image }
                _             => { _placeholder: "" }
            }
        ))

        nu $chart_path ($chart_context | to nuon)
        print ""
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

    print "Checking cert-issuer's TLS identity..."
    check_cert_issuer_identity $output_dir $cert_issuer_url
    print ""

    print "Checking CA provisioning consistency..."
    check_ca_provisioning $output_dir $cert_issuer_ca_configmap
    print ""

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
