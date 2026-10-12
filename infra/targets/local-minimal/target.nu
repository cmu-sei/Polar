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

        required_env : []

        overrides : ($target_dir | path join "overrides.dhall")

        charts : [
            ($layers | path join "1-platform/infra/namespaces/main.nu")
            ($layers | path join "1-platform/infra/storage/main.nu")
            ($layers | path join "2-services/cert-issuer/main.nu")
            ($layers | path join "2-services/jaeger/main.nu")
            ($layers | path join "2-services/ace/main.nu")
            ($layers | path join "2-services/neo4j/main.nu")
            ($layers | path join "2-services/postgres/main.nu")
            ($layers | path join "2-services/keycloak/main.nu")
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
            "postgres-pvc.yaml"
            "postgres-service.yaml"
            "postgres-statefulset.yaml"
            "keycloak-service.yaml"
            "keycloak-deployment.yaml"
            "cassini-resources.yaml"
            "cassini.yaml"
            "credentials-agent-resources.yaml"
            "credentials-agent.yaml"
            "credentials-agent-observer-resources.yaml"
            "credentials-agent-observer.yaml"
            "jaeger.yaml"
            "ace-rbac.yaml"
            "ace.yaml"
            "kube-observer-resources.yaml"
            "kube-observer.yaml"
            "kube-consumer-resources.yaml"
            "kube-consumer.yaml"
            "jira-observer-resources.yaml"
            "jira-observer.yaml"
            "jira-processor-resources.yaml"
            "jira-processor.yaml"
            "oci-registry-resolver-resources.yaml"
            "oci-registry-resolver.yaml"
            "git-repo-observer-resources.yaml"
            "git-repo-observer.yaml"
            "git-repo-processor-resources.yaml"
            "git-repo-processor.yaml"
            "git-repo-scheduler-resources.yaml"
            "git-repo-scheduler.yaml"
            "polar-git-server-resources.yaml"
            "polar-git-server.yaml"
            "polar-scheduler-observer-resources.yaml"
            "polar-scheduler-observer.yaml"
            "polar-scheduler-resources.yaml"
            "polar-scheduler.yaml"
            "secret-harness-owner-resources.yaml"
            "secret-harness-owner.yaml"
            "secret-harness-intruder-resources.yaml"
            "secret-harness-intruder.yaml"
        ]

        neo4j_bolt_addr       : "bolt+s://polar-db-svc.polar-graph.svc.cluster.local:7687"
        enable_tls            : true
        cert_issuer_url       : "https://cert-issuer.polar.svc.cluster.local:8443"
        cert_issuer_audience  : "polar-cert-issuer.local"
        # Name of the ConfigMap `polar-admin-grant bootstrap-ca` creates. Non-empty
        # means callers verify cert-issuer against it from their first request,
        # and render.nu then requires cert-issuer's ca.provisioning to be
        # "provisioned" (check_ca_provisioning).
        cert_issuer_ca_configmap : "polar-ca"
        polar_init_image      : "10.0.2.2:5000/polar-init:latest"
        identity_lifetime_overrides : [
            { identity: "graph.polar-graph.serviceaccount.cluster.local", lifetimeSecs: 31536000 }
            { identity: "ace.polar.serviceaccount.cluster.local", lifetimeSecs: 31536000 }
        ]
    } | to nuon
}
