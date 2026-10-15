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

    # A file that will not decrypt matters only if it leaves a secret the cluster needs unmade.
    let checked = (secrets_check $target_dir (existing_required_secrets))
    let secrets = $checked.secrets
    let failures = $checked.failures
    if ($checked.problems | is-not-empty) {
        for p in $checked.problems { print -e $"  FAILED: ($p)" }
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

    # The token that lets a client ask the broker to shut down. Made here, once; a re-run finds it
    # and leaves it. Read it with: kubectl -n polar get secret cassini-shutdown-token -o jsonpath='{.data.token}' | base64 -d
    kubectl_upsert_secret "cassini-shutdown-token" "polar" [$"token=(random_token)"]

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

    print ""
    print "  secrets: done"
    print ""
}
