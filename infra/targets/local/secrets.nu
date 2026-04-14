#!/usr/bin/env nu
# infra/targets/local/secrets.nu
#
# Creates Kubernetes secrets and ConfigMaps required by Polar agents.
# Reads encrypted secrets from secrets.sops.yaml (shared team secrets)
# and user.sops.yaml (user-specific secrets), decrypting with SOPS.
#
# Called by render.nu --apply after namespaces are created.
#
# Prerequisites:
#   - Namespaces must exist (polar, polar-graph)
#   - SOPS_AGE_BINARY and SOPS_AGE_SSH_PRIVATE_KEY_FILE must be set
#     (configured automatically by the dev shell)
#   - .sops.yaml must exist in this directory
#   - secrets.sops.yaml must exist and be encrypted
#
# Usage:
#   nu infra/targets/local/secrets.nu <target_dir> <repo_root>

def kubectl_upsert_secret [
    name: string
    namespace: string
    --from-literal: list<string> = []
    --from-file: list<string> = []
] {
    # Check if secret exists
    let exists = (
        do { run-external "kubectl" "get" "secret" $name "-n" $namespace } | complete | get exit_code
    ) == 0

    if $exists {
        print $"    secret/($name) already exists in ($namespace) — skipping"
        return
    }

    mut args = ["create" "secret" "generic" $name "-n" $namespace]

    for lit in $from_literal {
        $args = ($args | append ["--from-literal" $lit])
    }

    for f in $from_file {
        $args = ($args | append ["--from-file" $f])
    }

    run-external "kubectl" ...$args
    print $"    secret/($name) created in ($namespace)"
}

def kubectl_upsert_configmap [
    name: string
    namespace: string
    --from-file: list<string> = []
    --from-literal: list<string> = []
] {
    let exists = (
        do { run-external "kubectl" "get" "configmap" $name "-n" $namespace } | complete | get exit_code
    ) == 0

    if $exists {
        print $"    configmap/($name) already exists in ($namespace) — skipping"
        return
    }

    mut args = ["create" "configmap" $name "-n" $namespace]

    for f in $from_file {
        $args = ($args | append ["--from-file" $f])
    }

    for lit in $from_literal {
        $args = ($args | append ["--from-literal" $lit])
    }

    run-external "kubectl" ...$args
    print $"    configmap/($name) created in ($namespace)"
}

def decrypt_sops [target_dir: string, file: string] {
    let path = ($target_dir | path join $file)

    if not ($path | path exists) {
        print $"    WARNING: ($file) not found — skipping"
        return {}
    }

    let sops_config = ($target_dir | path join ".sops.yaml")
    let user_sops_config = ($target_dir | path join ".user.sops.yaml")

    let config_file = if ($file == "user.sops.yaml") {
        $user_sops_config
    } else {
        $sops_config
    }

    if not ($config_file | path exists) {
        print $"    WARNING: SOPS config not found at ($config_file) — skipping ($file)"
        return {}
    }

    do {
        run-external "sops" "--decrypt" "--output-type" "json" $path
        | from json
    } | default {}
}

def main [target_dir: string, repo_root: string] {
    print "── secrets ──"
    print "  Decrypting secrets..."

    # Decrypt shared and user secrets
    let shared  = (decrypt_sops $target_dir "secrets.sops.yaml")
    let user    = (decrypt_sops $target_dir "user.sops.yaml")

    # Merge — user values take precedence over shared
    let secrets = ($shared | merge $user)

    print "  Creating secrets and ConfigMaps..."
    print ""

    # ── polar-graph namespace ─────────────────────────────────────────────────

    print "  [polar-graph]"

    # Neo4j auth secret
    if ($secrets | get -i neo4j_auth | is-not-empty) {
        kubectl_upsert_secret "neo4j-secret" "polar-graph"
            --from-literal [$"secret=($secrets.neo4j_auth)"]
    }

    print ""

    # ── polar namespace ───────────────────────────────────────────────────────

    print "  [polar]"

    # Graph password
    if ($secrets | get -i graph_password | is-not-empty) {
        kubectl_upsert_secret "polar-graph-pw" "polar"
            --from-literal [$"secret=($secrets.graph_password)"]
    }

    # GitLab token
    if ($secrets | get -i gitlab_token | is-not-empty) {
        kubectl_upsert_secret "gitlab-secret" "polar"
            --from-literal [$"token=($secrets.gitlab_token)"]
    } else {
        print "    WARNING: gitlab_token empty — gitlab-secret not created"
    }

    # Jira token
    if ($secrets | get -i jira_token | is-not-empty) {
        kubectl_upsert_secret "jira-secret" "polar"
            --from-literal [$"token=($secrets.jira_token)"]
    } else {
        # Create placeholder so jira agents can start
        kubectl_upsert_secret "jira-secret" "polar"
            --from-literal ["token=placeholder"]
    }

    # OCI registry auth
    if ($secrets | get -i docker_auth_json | is-not-empty) {
        kubectl_upsert_secret "oci-registry-auth" "polar"
            --from-literal [$"oci-registry-auth=($secrets.docker_auth_json)"]
    } else {
        print "    WARNING: docker_auth_json empty — oci-registry-auth not created"
    }

    # Neo4j bolt CA — ConfigMap, not a Secret (public key material)
    let ca_cert_path = (
        $secrets
        | get -i neo4j_ca_cert_path
        | default "result-tlsCerts/neo4j/ca_certificate.pem"
    )
    let full_ca_path = ($repo_root | path join $ca_cert_path)

    if ($full_ca_path | path exists) {
        kubectl_upsert_configmap "neo4j-bolt-ca" "polar"
            --from-file [$"ca.pem=($full_ca_path)"]
    } else {
        print $"    WARNING: neo4j CA cert not found at ($full_ca_path)"
        print "    Run: nix build .#tlsCerts first"
    }

    # Apply jira agent certificate — cert-manager doesn't always create this
    # on fresh cluster apply
    let jira_cert = ($repo_root | path join "manifests/jira-agent-cert.yaml")
    if ($jira_cert | path exists) {
        print "    applying jira-agent-cert.yaml..."
        run-external "kubectl" "apply" "-f" $jira_cert
    }

    print ""
    print "  secrets: done"
    print ""
}
