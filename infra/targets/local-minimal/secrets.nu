#!/usr/bin/env nu
# infra/targets/local-minimal/secrets.nu
#
# Creates Kubernetes secrets and ConfigMaps required by the minimal Polar
# profile: neo4j, cassini, cert-issuer, jaeger, jira, kube agents.
#
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

    let jira_token = ($secrets | get -o jira_token | default "")
    if ($jira_token | is-not-empty) {
        kubectl_upsert_secret "jira-secret" "polar" [$"token=($jira_token)"]
    } else {
        kubectl_upsert_secret "jira-secret" "polar" ["token=placeholder"]
    }

    # Registry credentials for the OCI resolver: an empty Docker credential file, so registries are
    # asked anonymously. Put real ones in the Secret to pull from a private registry.
    kubectl_upsert_secret "oci-registry-auth" "polar" ["oci-registry-auth={}"]

    print ""

    # -- polar-auth namespace ------------------------------------------------
    print "  [polar-auth]"

    let keycloak_admin_user = ($secrets | get -o keycloak_admin_user | default "admin")
    let keycloak_admin_password = ($secrets | get -o keycloak_admin_password | default "")
    if ($keycloak_admin_password | is-not-empty) {
        kubectl_upsert_secret "keycloak-admin-secret" "polar-auth" [
            $"username=($keycloak_admin_user)"
            $"password=($keycloak_admin_password)"
        ]
    } else if not (secret_exists "keycloak-admin-secret" "polar-auth") {
        print "    WARNING: keycloak_admin_password not set -- skipping keycloak-admin-secret"
    }

    let keycloak_db_password = ($secrets | get -o keycloak_db_password | default "")
    if ($keycloak_db_password | is-not-empty) {
        kubectl_upsert_secret "keycloak-db-secret" "polar-auth" [$"password=($keycloak_db_password)"]
    } else if not (secret_exists "keycloak-db-secret" "polar-auth") {
        print "    WARNING: keycloak_db_password not set -- skipping keycloak-db-secret"
    }

    print ""
    print "  secrets: done"
    print ""
}
