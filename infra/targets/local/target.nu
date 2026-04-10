#!/usr/bin/env nu
# infra/targets/local/target.nu
#
# Local deployment target configuration.
# Cluster: nix-usernetes (rootless podman + containerd)
# Kubeconfig: ~/Documents/projects/nix-usernetes/kubeconfig
#
# Returns a config record consumed by render.nu.
# May run setup logic before returning (cert loading, path resolution, etc.)

def main [] {
    let repo_root  = (git rev-parse --show-toplevel | str trim)
    let infra_root = ($repo_root | path join "infra")
    let target_dir = ($infra_root | path join "targets/local")
    let layers     = ($infra_root | path join "layers")

    {
        # ── Identity ──────────────────────────────────────────────────────────
        name       : "local"
        repo_root  : $repo_root
        infra_root : $infra_root
        target_dir : $target_dir
        output_dir : ($repo_root | path join "manifests")

        # ── Required environment variables ────────────────────────────────────
        # Validated by validate.nu before any rendering begins.
        required_env : [
            "GRAPH_PASSWORD"
            "NEO4J_AUTH"
            "NEO4J_TLS_CA_CERT_CONTENT"
            "NEO4J_TLS_SERVER_CERT_CONTENT"
            "NEO4J_TLS_SERVER_KEY_CONTENT"
            "GITLAB_TOKEN"
            "DOCKER_AUTH_JSON"
            "POLAR_SCHEDULER_REMOTE_URL"
        ]

        # ── Overrides path ────────────────────────────────────────────────────
        overrides : ($target_dir | path join "overrides.dhall")

        # ── Chart list ────────────────────────────────────────────────────────
        # Ordered by dependency. render.nu calls each main.nu in this order.
        # Cross-service outputs (neo4jBoltAddr, jaegerDNSName) are resolved
        # after their chart renders and injected into dependent chart contexts.
        charts : [
            # Layer 1 — platform infra
            ($layers | path join "1-platform/infra/namespaces/main.nu")
            ($layers | path join "1-platform/infra/storage/main.nu")
            ($layers | path join "1-platform/services/cert-manager/main.nu")

            # Layer 2 — services
            ($layers | path join "2-services/jaeger/main.nu")
            ($layers | path join "2-services/neo4j/main.nu")
            ($layers | path join "2-services/cassini/main.nu")

            # Layer 3 — workloads
            ($layers | path join "3-workloads/agents/gitlab/main.nu")
            ($layers | path join "3-workloads/agents/kube/main.nu")
            ($layers | path join "3-workloads/agents/git/main.nu")
            ($layers | path join "3-workloads/agents/provenance/main.nu")
            ($layers | path join "3-workloads/agents/build/main.nu")
            ($layers | path join "3-workloads/agents/scheduler/main.nu")
        ]

        # ── Apply order ───────────────────────────────────────────────────────
        # kubectl apply runs in this order. Matches the old cluster-apply
        # recipe in the Justfile, now explicit and target-owned.
        # Namespaces and storage first, then PKI chain, then workloads.
        apply_order : [
            "namespaces.yaml"
            "local-path-provisioner.yaml"
            "storage-class.yaml"
            "ca-issuer.yaml"
            "mtls-ca.yaml"
            "leaf-issuer.yaml"
            "neo4j-configmap.yaml"
            "neo4j-init-script-configmap.yaml"
            "neo4j-pvcs.yaml"
            "neo4j-service.yaml"
            "neo4j-statefulset.yaml"
            "cassini-server-cert.yaml"
            "cassini-client-cert.yaml"
            "cassini.yaml"
            "jaeger.yaml"
            # Agent certs before deployments
            "gitlab-agent-cert.yaml"
            "kube-agent-cert.yaml"
            "git-agent-cert.yaml"
            "provenance-agent-cert.yaml"
            "build-agent-cert.yaml"
            "scheduler-agent-cert.yaml"
            # RBAC before deployments that need it
            "kube-agent-rbac.yaml"
            "build-agent-rbac.yaml"
            # Workloads
            "gitlab-agents.yaml"
            "kube-agent.yaml"
            "git-agents.yaml"
            "provenance-agents.yaml"
            "build-agents.yaml"
            "polar-scheduler.yaml"
        ]

        # ── Cross-service resolved values ─────────────────────────────────────
        # These are computed here and injected into chart contexts by render.nu.
        neo4j_bolt_addr   : "bolt+s://polar-db-svc.polar-graph.svc.cluster.local:7687"
        jaeger_dns_name   : "http://jaeger-svc.polar.svc.cluster.local:16686/v1/traces"
        scheduler_remote_url : ($env.POLAR_SCHEDULER_REMOTE_URL? | default "https://github.com/daveman1010221/polar-schedules.git")

        # ── Target flags ──────────────────────────────────────────────────────
        enable_tls : true
    } | to nuon
}
