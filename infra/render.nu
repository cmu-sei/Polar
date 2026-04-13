#!/usr/bin/env nu
# infra/render.nu
#
# Polar infrastructure rendering pipeline.
# Reads target configuration, validates environment, renders all charts.
#
# Usage:
#   nu infra/render.nu <target>
#   nu infra/render.nu local
#   nu infra/render.nu sandbox
#
# Output goes to the directory specified in target.nu's output_dir.
# After rendering, apply with:
#   nu infra/render.nu local --apply

use validate.nu *

def main [
    target: string        # Target name: local, sandbox, ace
    --apply               # Apply rendered manifests after rendering
    --dry-run             # Render but do not apply
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
                "cassini"    => { jaegerDNSName: $jaeger_dns_name }
                "gitlab"     => { neo4jBoltAddr: $neo4j_bolt_addr }
                "kube"       => { neo4jBoltAddr: $neo4j_bolt_addr }
                "git"        => { neo4jBoltAddr: $neo4j_bolt_addr }
                "jira"       => { neo4jBoltAddr: $neo4j_bolt_addr }
                "provenance" => { neo4jBoltAddr: $neo4j_bolt_addr }
                "build"      => { neo4jBoltAddr: $neo4j_bolt_addr }
                "scheduler"  => { neo4jBoltAddr: $neo4j_bolt_addr, remoteUrl: $scheduler_remote_url }
                _            => { _placeholder: "" }
            }
        ))

        nu $chart_path ($chart_context | to nuon)
        print ""
    }

    print "=== Render complete ==="
    print $"  output: ($output_dir)"
    print ""

    # ── Apply ─────────────────────────────────────────────────────────────────
    if $apply and not $dry_run {
        print "Applying manifests..."
        apply_manifests $config $output_dir
    } else if $dry_run {
        print "(dry-run: skipping apply)"
    } else {
        print "To apply:"
        print $"  nu infra/render.nu ($target) --apply"
        print ""
        print "Or apply manually in order:"
        for f in $config.apply_order {
            print $"  kubectl apply -f ($output_dir)/($f)"
        }
    }
}

# Apply rendered manifests in the order declared by target.nu
def apply_manifests [config: record, output_dir: string] {
    let kubeconfig = (
        $env.KUBECONFIG?
        | default ($env.HOME | path join "Documents/projects/nix-usernetes/kubeconfig")
    )

    let kc = $"kubectl --kubeconfig ($kubeconfig)"

    for filename in $config.apply_order {
        let manifest = ($output_dir | path join $filename)
        if ($manifest | path exists) {
            print $"  applying ($filename)..."
            run-external "kubectl" "--kubeconfig" $kubeconfig "apply" "-f" $manifest
        } else {
            print $"  skipping ($filename) — not found"
        }
    }

    # SA token ordering fix — retry agents manifest after brief pause
    # to allow token controller to populate SA token secrets.
    print ""
    print "  waiting 5s for SA token controller..."
    sleep 5sec
    for filename in ["kube-agent-rbac.yaml", "build-agent-rbac.yaml"] {
        let manifest = ($output_dir | path join $filename)
        if ($manifest | path exists) {
            print $"  retrying ($filename)..."
            run-external "kubectl" "--kubeconfig" $kubeconfig "apply" "-f" $manifest
        }
    }

    print ""
    print "Apply complete."
}
