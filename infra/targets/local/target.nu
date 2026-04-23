#!/usr/bin/env nu
# infra/targets/local/target.nu

def main [] {
    let repo_root  = (git rev-parse --show-toplevel | str trim)
    let infra_root = ($repo_root | path join "infra")
    let target_dir = ($infra_root | path join "targets/local")
    let layers     = ($infra_root | path join "layers")

    {
        name       : "local"
        repo_root  : $repo_root
        infra_root : $infra_root
        target_dir : $target_dir
        output_dir : ($repo_root | path join "manifests")

        required_env : [
            "NEO4J_TLS_CA_CERT_CONTENT"
            "NEO4J_TLS_SERVER_CERT_CONTENT"
            "NEO4J_TLS_SERVER_KEY_CONTENT"
            "POLAR_SCHEDULER_REMOTE_URL"
        ]

        overrides : ($target_dir | path join "overrides.dhall")

        charts : [
            ($layers | path join "1-platform/infra/namespaces/main.nu")
            ($layers | path join "1-platform/infra/storage/main.nu")
            ($layers | path join "1-platform/services/cert-manager/main.nu")
            ($layers | path join "2-services/jaeger/main.nu")
            ($layers | path join "2-services/neo4j/main.nu")
            ($layers | path join "2-services/cassini/main.nu")
            ($layers | path join "3-workloads/agents/gitlab/main.nu")
            ($layers | path join "3-workloads/agents/kube/main.nu")
            ($layers | path join "3-workloads/agents/git/main.nu")
            ($layers | path join "3-workloads/agents/jira/main.nu")
            ($layers | path join "3-workloads/agents/provenance/main.nu")
            ($layers | path join "3-workloads/agents/build/main.nu")
            ($layers | path join "3-workloads/agents/scheduler/main.nu")
            ($layers | path join "3-workloads/agents/openapi/main.nu")
        ]

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
            "gitlab-agent-cert.yaml"
            "kube-agent-cert.yaml"
            "git-agent-cert.yaml"
            "provenance-agent-cert.yaml"
            "build-agent-cert.yaml"
            "scheduler-agent-cert.yaml"
            "kube-agent-rbac.yaml"
            "build-agent-rbac.yaml"
            "gitlab-observer.yaml"
            "gitlab-consumer.yaml"
            "kube-observer.yaml"
            "kube-consumer.yaml"
            "git-observer.yaml"
            "git-consumer.yaml"
            "git-scheduler.yaml"
            "jira-observer.yaml"
            "jira-processor.yaml"
            "provenance-linker.yaml"
            "provenance-resolver.yaml"
            "build-orchestrator.yaml"
            "build-processor.yaml"
            "scheduler-observer.yaml"
            "scheduler-processor.yaml"
            "openapi-agent-cert.yaml"
            "openapi-observer.yaml"
            "openapi-processor.yaml"
        ]

        neo4j_bolt_addr      : "bolt+s://polar-db-svc.polar-graph.svc.cluster.local:7687"
        jaeger_dns_name      : "http://jaeger-svc.polar.svc.cluster.local:16686/v1/traces"
        scheduler_remote_url : ($env.POLAR_SCHEDULER_REMOTE_URL? | default "https://github.com/daveman1010221/polar-schedules.git")
        enable_tls           : true
    } | to nuon
}
