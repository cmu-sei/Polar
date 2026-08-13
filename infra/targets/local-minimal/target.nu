#!/usr/bin/env nu
# infra/targets/local-minimal/target.nu

def main [] {
    let repo_root  = (git rev-parse --show-toplevel | str trim)
    let infra_root = ($repo_root | path join "infra")
    let target_dir = ($infra_root | path join "targets/local-minimal")
    let layers     = ($infra_root | path join "layers")

    {
        name       : "local-minimal"
        repo_root  : $repo_root
        infra_root : $infra_root
        target_dir : $target_dir
        output_dir : ($repo_root | path join "manifests")

        required_env : [
            "NEO4J_TLS_CA_CERT_CONTENT"
            "NEO4J_TLS_SERVER_CERT_CONTENT"
            "NEO4J_TLS_SERVER_KEY_CONTENT"
        ]

        overrides : ($target_dir | path join "overrides.dhall")

        charts : [
            ($layers | path join "1-platform/infra/namespaces/main.nu")
            ($layers | path join "1-platform/infra/storage/main.nu")
            ($layers | path join "2-services/cert-issuer/main.nu")
            ($layers | path join "2-services/jaeger/main.nu")
            ($layers | path join "2-services/neo4j/main.nu")
            ($layers | path join "2-services/cassini/main.nu")
            ($layers | path join "3-workloads/agents/kube/main.nu")
            ($layers | path join "3-workloads/agents/jira/main.nu")
        ]

        apply_order : [
            "namespaces.yaml"
            "local-path-provisioner.yaml"
            "storage-class.yaml"
            "cert-issuer-pvc.yaml"
            "cert-issuer-configmap.yaml"
            "cert-issuer-service.yaml"
            "cert-issuer-deployment.yaml"
            "neo4j-configmap.yaml"
            "neo4j-init-script-configmap.yaml"
            "neo4j-pvcs.yaml"
            "neo4j-service.yaml"
            "neo4j-statefulset.yaml"
            "cassini.yaml"
            "jaeger.yaml"
            "kube-agent-rbac.yaml"
            "kube-observer.yaml"
            "kube-consumer.yaml"
            "jira-observer.yaml"
            "jira-processor.yaml"
        ]

        neo4j_bolt_addr       : "bolt+s://polar-db-svc.polar-graph.svc.cluster.local:7687"
        jaeger_dns_name       : "http://jaeger-svc.polar.svc.cluster.local:16686/v1/traces"
        scheduler_remote_url  : ""
        enable_tls            : true
        cert_issuer_url       : "http://cert-issuer.polar.svc.cluster.local:8443"
        cert_issuer_audience  : "polar-cert-issuer.local"
        cert_client_image     : "polar-cert-client:latest"
        polar_init_image      : "polar-init:latest"
        cassini_shutdown_token : ($env.CASSINI_SHUTDOWN_TOKEN? | default "HEYTHERE")
        identity_lifetime_overrides : [
            { identity: "default.polar-graph.serviceaccount.cluster.local", lifetimeSecs: 31536000 }
        ]
    } | to nuon
}
