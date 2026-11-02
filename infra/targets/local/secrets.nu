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

# infra/targets/local/secrets.nu
#
# Creates Kubernetes secrets and ConfigMaps required by Polar agents.
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

    let gitlab_token = ($secrets | get -o gitlab_token | default "")
    if ($gitlab_token | is-not-empty) {
        kubectl_sops_secret "gitlab-secret" "polar" [$"token=($gitlab_token)"]
    } else {
        kubectl_sops_secret "gitlab-secret" "polar" ["token=placeholder"] --placeholder
    }

    # No jira-secret since 3.0.0: jira-observer's credential is its signed `credentials` runtime section
    # (docs/architecture/jira.md, "The credential"). The `jira_token` of the sops files is not read here: up.nu's
    # seed seals it once into that section on a fresh cluster (infra/seed.nu), and polar-sign changes it after.

    let docker_auth_json = ($secrets | get -o docker_auth_json | default "")
    if ($docker_auth_json | is-not-empty) {
        kubectl_sops_secret "oci-registry-auth" "polar" [$"oci-registry-auth=($docker_auth_json)"]
    } else {
        kubectl_sops_secret "oci-registry-auth" "polar" ["oci-registry-auth={}"] --placeholder
    }

    print ""
    print "  secrets: done"
    print ""
}
