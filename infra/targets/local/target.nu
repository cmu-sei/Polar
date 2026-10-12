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

        required_env : []

        overrides : ($target_dir | path join "overrides.dhall")

        charts : [
            ($layers | path join "1-platform/infra/namespaces/main.nu")
            ($layers | path join "1-platform/infra/storage/main.nu")
            ($layers | path join "2-services/cert-issuer/main.nu")
            ($layers | path join "2-services/jaeger/main.nu")
            ($layers | path join "2-services/neo4j/main.nu")
            ($layers | path join "3-workloads/agents/gitlab/main.nu")
            ($layers | path join "3-workloads/agents/build-processor/main.nu")
            ($layers | path join "3-workloads/agents/openapi/main.nu")
        ]

        apply_order : [
            "namespaces.yaml"
            "serviceaccounts.yaml"
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
            "cassini-resources.yaml"
            "cassini.yaml"
            "credentials-agent-resources.yaml"
            "credentials-agent.yaml"
            "credentials-agent-observer-resources.yaml"
            "credentials-agent-observer.yaml"
            "jaeger.yaml"
            "kube-observer-resources.yaml"
            "gitlab-observer.yaml"
            "gitlab-consumer.yaml"
            "kube-observer.yaml"
            "kube-consumer-resources.yaml"
            "kube-consumer.yaml"
            "git-repo-observer-resources.yaml"
            "git-repo-observer.yaml"
            "git-repo-processor-resources.yaml"
            "git-repo-processor.yaml"
            "git-repo-scheduler-resources.yaml"
            "git-repo-scheduler.yaml"
            "jira-observer-resources.yaml"
            "jira-observer.yaml"
            "jira-processor-resources.yaml"
            "jira-processor.yaml"
            "build-processor.yaml"
            "oci-registry-resolver-resources.yaml"
            "oci-registry-resolver.yaml"
            "polar-git-server-resources.yaml"
            "polar-git-server.yaml"
            "polar-scheduler-observer-resources.yaml"
            "polar-scheduler-observer.yaml"
            "polar-scheduler-resources.yaml"
            "polar-scheduler.yaml"
            "openapi-observer.yaml"
            "openapi-processor.yaml"
        ]

        neo4j_bolt_addr       : "bolt+s://polar-db-svc.polar-graph.svc.cluster.local:7687"
        identity_lifetime_overrides : [
            { identity: "graph.polar-graph.serviceaccount.cluster.local", lifetimeSecs: 31536000 }
        ]
        enable_tls            : true
        cert_issuer_url       : "https://cert-issuer.polar.svc.cluster.local:8443"
        cert_issuer_audience  : "polar-cert-issuer.local"
        # Name of the ConfigMap `polar-admin-grant bootstrap-ca` creates. Non-empty
        # means callers verify cert-issuer against it from their first request,
        # and render.nu then requires cert-issuer's ca.provisioning to be
        # "provisioned" (check_ca_provisioning).
        cert_issuer_ca_configmap : "polar-ca"
        polar_init_image      : "polar-init:latest"
    } | to nuon
}
