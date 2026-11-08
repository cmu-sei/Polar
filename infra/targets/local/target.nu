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
            # cert-issuer, a catalog workload of kind `ca` (config/workloads/cert-issuer.nuon)
            "cert-issuer-resources.yaml"
            "cert-issuer.yaml"
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
            "git-repo-controller-resources.yaml"
            "git-repo-controller.yaml"
            "soft-state-observer-resources.yaml"
            "soft-state-observer.yaml"
            "soft-state-processor-resources.yaml"
            "soft-state-processor.yaml"
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

        # Rendered and never applied: the load harness's Jobs, which infra/load-harness.nu copies, one per producer and sink of a
        # run, when the target enables the harness.
        rendered_only : ["harness-producer.yaml" "harness-sink.yaml"]

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
