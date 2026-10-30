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

# infra/targets/local-minimal/secrets.nu
#
# Creates Kubernetes secrets and ConfigMaps required by the minimal Polar
# profile: neo4j, cassini, cert-issuer, jaeger, jira, kube agents.
#
# Reads encrypted secrets from secrets.sops.yaml (shared team secrets)
# and user.sops.yaml (user-specific secrets), decrypting with SOPS.
# A Secret made from them is updated when they change; one made here (the
# shutdown token) is made once and kept (infra/secrets-lib.nu).
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
    # A value stored base64-encoded would reach the workload still encoded: Kubernetes encodes it again.
    for w in $checked.warnings { print $"    WARNING: ($w)" }

    print "  Creating and updating secrets..."
    print ""

    # ── polar-graph namespace ─────────────────────────────────────────────────
    print "  [polar-graph]"

    let neo4j_auth = ($secrets | get -o neo4j_auth | default "")
    if ($neo4j_auth | is-not-empty) {
        kubectl_sops_secret "neo4j-secret" "polar-graph" [$"secret=($neo4j_auth)"]
    }

    print ""

    # ── polar namespace ───────────────────────────────────────────────────────
    print "  [polar]"

    # The token that lets a client ask the broker to shut down. Made here, once; a re-run finds it
    # and leaves it. Read it with: kubectl -n polar get secret cassini-shutdown-token -o jsonpath='{.data.token}' | base64 -d
    kubectl_generated_secret "cassini-shutdown-token" "polar" [$"token=(random_token)"]

    let graph_password = ($secrets | get -o graph_password | default "")
    if ($graph_password | is-not-empty) {
        kubectl_sops_secret "polar-graph-pw" "polar" [$"secret=($graph_password)"]
    }

    let jira_token = ($secrets | get -o jira_token | default "")
    if ($jira_token | is-not-empty) {
        kubectl_sops_secret "jira-secret" "polar" [$"token=($jira_token)"]
    } else {
        kubectl_sops_secret "jira-secret" "polar" ["token=placeholder"] --placeholder
    }

    # Registry credentials for the OCI resolver: an empty Docker credential file, so registries are
    # asked anonymously. Put real ones in the Secret to pull from a private registry; a re-run keeps them.
    kubectl_sops_secret "oci-registry-auth" "polar" ["oci-registry-auth={}"] --placeholder

    print ""

    # -- polar-auth namespace ------------------------------------------------
    print "  [polar-auth]"

    let keycloak_admin_user = ($secrets | get -o keycloak_admin_user | default "admin")
    let keycloak_admin_password = ($secrets | get -o keycloak_admin_password | default "")
    if ($keycloak_admin_password | is-not-empty) {
        kubectl_sops_secret "keycloak-admin-secret" "polar-auth" [
            $"username=($keycloak_admin_user)"
            $"password=($keycloak_admin_password)"
        ]
    } else if not (secret_exists "keycloak-admin-secret" "polar-auth") {
        print "    WARNING: keycloak_admin_password not set -- skipping keycloak-admin-secret"
    }

    let keycloak_db_password = ($secrets | get -o keycloak_db_password | default "")
    if ($keycloak_db_password | is-not-empty) {
        kubectl_sops_secret "keycloak-db-secret" "polar-auth" [$"password=($keycloak_db_password)"]
    } else if not (secret_exists "keycloak-db-secret" "polar-auth") {
        print "    WARNING: keycloak_db_password not set -- skipping keycloak-db-secret"
    }

    print ""
    print "  secrets: done"
    print ""
}
