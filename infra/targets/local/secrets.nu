#!/usr/bin/env nu
# infra/targets/local/secrets.nu
#
# Creates Kubernetes secrets and ConfigMaps required by Polar agents.
# Reads encrypted secrets from secrets.sops.yaml (shared team secrets)
# and user.sops.yaml (user-specific secrets), decrypting with SOPS.
#
# Called by infra/up.nu after namespaces are created.

use ../../secrets-lib.nu *

def main [target_dir: string, repo_root: string] {
    print "── secrets ──"
    print "  Decrypting secrets..."

    let shared = (decrypt_sops $target_dir "secrets.sops.yaml")
    let user   = (decrypt_sops $target_dir "user.sops.yaml")
    let failures = ([$shared.failure $user.failure] | compact)

    # Merge — user values take precedence
    let secrets = ($shared.data | merge $user.data)

    # A file that will not decrypt matters only if it leaves a secret the cluster needs unmade.
    let required = [
        { namespace: "polar-graph", name: "neo4j-secret", key: "neo4j_auth" }
        { namespace: "polar", name: "polar-graph-pw", key: "graph_password" }
    ]
    let existing = ($required
        | where {|r| secret_exists $r.name $r.namespace }
        | each {|r| $"($r.namespace)/($r.name)" })
    let problems = (required_secret_problems $required $existing $secrets $failures)
    if ($problems | is-not-empty) {
        for p in $problems { print -e $"  FAILED: ($p)" }
        exit 1
    }
    for f in $failures {
        print $"    note: could not decrypt ($f.file) \(($f.reason)\); every secret needed already exists"
    }

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

    # git-config — StaticCredentialConfig JSON for the git observer
    let git_config = ($target_dir | path join "conf/git.json")
    if ($git_config | path exists) {
        kubectl_upsert_secret "git-config" "polar" [] [$"git.json=($git_config)"]
    } else {
        print $"    WARNING: git.json not found at ($target_dir)/conf/git.json — git observer will not start"
    }

    print ""
    print "  secrets: done"
    print ""
}
