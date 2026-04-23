#!/usr/bin/env nu
# infra/targets/local/secrets.nu
#
# Creates Kubernetes secrets and ConfigMaps required by Polar agents.
# Reads encrypted secrets from secrets.sops.yaml (shared team secrets)
# and user.sops.yaml (user-specific secrets), decrypting with SOPS.
#
# Called by render.nu --apply after namespaces are created.

def kubectl_upsert_secret [
    name: string
    namespace: string
    literals: list<string> = []
    files: list<string> = []
] {
    let exists = (
        do { run-external "kubectl" "get" "secret" $name "-n" $namespace }
        | complete
        | get exit_code
    ) == 0

    if $exists {
        print $"    secret/($name) already exists in ($namespace) — skipping"
        return
    }

    mut args = ["create" "secret" "generic" $name "-n" $namespace]
    for lit in $literals {
        $args = ($args | append ["--from-literal" $lit])
    }
    for f in $files {
        $args = ($args | append ["--from-file" $f])
    }

    run-external "kubectl" ...$args
    print $"    secret/($name) created in ($namespace)"
}

def kubectl_upsert_configmap [
    name: string
    namespace: string
    files: list<string> = []
    literals: list<string> = []
] {
    let exists = (
        do { run-external "kubectl" "get" "configmap" $name "-n" $namespace }
        | complete
        | get exit_code
    ) == 0

    if $exists {
        print $"    configmap/($name) already exists in ($namespace) — skipping"
        return
    }

    mut args = ["create" "configmap" $name "-n" $namespace]
    for f in $files {
        $args = ($args | append ["--from-file" $f])
    }
    for lit in $literals {
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

    let sops_config = if ($file == "user.sops.yaml") {
        ($target_dir | path join ".user.sops.yaml")
    } else {
        ($target_dir | path join ".sops.yaml")
    }

    if not ($sops_config | path exists) {
        print $"    WARNING: SOPS config not found at ($sops_config) — skipping ($file)"
        return {}
    }

    let result = (
        do {
            with-env { SOPS_CONFIG: $sops_config } {
                run-external "sops" "--decrypt" "--output-type" "json" $path
            }
        } | complete
    )

    if $result.exit_code != 0 {
        print $"    WARNING: failed to decrypt ($file) — ($result.stderr)"
        return {}
    }

    $result.stdout | from json
}

def main [target_dir: string, repo_root: string] {
    print "── secrets ──"
    print "  Decrypting secrets..."

    let shared = (decrypt_sops $target_dir "secrets.sops.yaml")
    let user   = (decrypt_sops $target_dir "user.sops.yaml")

    # Merge — user values take precedence
    let secrets = ($shared | merge $user)

    print "  Creating secrets and ConfigMaps..."
    print ""

    # ── polar-graph namespace ─────────────────────────────────────────────────
    print "  [polar-graph]"

    let neo4j_auth = ($secrets | get -o neo4j_auth | default "")
    if ($neo4j_auth | is-not-empty) {
        kubectl_upsert_secret "neo4j-secret" "polar-graph" [$"secret=($neo4j_auth)"]
    }

    print ""

    # ── polar namespace ───────────────────────────────────────────────────────
    print "  [polar]"

    let graph_password = ($secrets | get -o graph_password | default "")
    if ($graph_password | is-not-empty) {
        kubectl_upsert_secret "polar-graph-pw" "polar" [$"secret=($graph_password)"]
    }

    let gitlab_token = ($secrets | get -o gitlab_token | default "")
    if ($gitlab_token | is-not-empty) {
        kubectl_upsert_secret "gitlab-secret" "polar" [$"token=($gitlab_token)"]
    } else {
        kubectl_upsert_secret "gitlab-secret" "polar" ["token=placeholder"]
    }

    let jira_token = ($secrets | get -o jira_token | default "")
    if ($jira_token | is-not-empty) {
        kubectl_upsert_secret "jira-secret" "polar" [$"token=($jira_token)"]
    } else {
        kubectl_upsert_secret "jira-secret" "polar" ["token=placeholder"]
    }

    let docker_auth_json = ($secrets | get -o docker_auth_json | default "")
    if ($docker_auth_json | is-not-empty) {
        kubectl_upsert_secret "oci-registry-auth" "polar" [$"oci-registry-auth=($docker_auth_json)"]
    } else {
        kubectl_upsert_secret "oci-registry-auth" "polar" ["oci-registry-auth={}"]
    }

    # neo4j-bolt-ca — ConfigMap not Secret (public key material)
    let ca_cert_path = ($secrets | get -o neo4j_ca_cert_path | default "result-tls-certs/neo4j/ca_certificate.pem")
    let full_ca_path = ($repo_root | path join $ca_cert_path)

    if ($full_ca_path | path exists) {
        kubectl_upsert_configmap "neo4j-bolt-ca" "polar" [$"ca.pem=($full_ca_path)"]
    } else {
        print $"    WARNING: neo4j CA cert not found at ($full_ca_path)"
        print "    Run: nix build .#tlsCerts first"
    }

    # jira-agent-cert — cert-manager misses this on fresh apply
    let jira_cert = ($repo_root | path join "manifests/jira-agent-cert.yaml")
    if ($jira_cert | path exists) {
        print "    applying jira-agent-cert.yaml..."
        run-external "kubectl" "apply" "-f" $jira_cert
    }

    # build-orchestrator-config — cyclops.yaml as a secret
    let cyclops_yaml = ($target_dir | path join "conf/cyclops.yaml")
    if ($cyclops_yaml | path exists) {
        kubectl_upsert_secret "build-orchestrator-config" "polar" [] [$"cyclops.yaml=($cyclops_yaml)"]
    } else {
        print $"    WARNING: cyclops.yaml not found at ($cyclops_yaml) — build-orchestrator will not start"
    }

    print ""
    print "  secrets: done"
    print ""
}
