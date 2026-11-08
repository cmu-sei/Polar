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

# infra/tests/secrets.nu
#
# Tests of the decisions in infra/secrets-lib.nu. Whether the secrets step can go on when SOPS will
# not decrypt: it can when every secret the cluster needs is already there; it must stop, and say
# why, when one is missing and could not have been made. What it does with a Secret that exists: one
# from the SOPS files is updated when the files say something else, and kept when they hold no
# value; one made here is made once and never compared. And which SOPS values look base64-encoded,
# which Kubernetes would encode a second time. No cluster is needed: kubectl is only at the edge.
#
#   nu infra/tests/secrets.nu

use std/assert
use ../secrets-lib.nu *

# A string of printable ASCII with no whitespace, `length` long.
def printable [length: int] {
    seq 1 $length | each { char --integer (random int 33..126) } | str join
}

# The four spellings of base64 a value may be stored in.
def spellings [text: string] {
    [
        ($text | encode base64)
        ($text | encode base64 --nopad)
        ($text | encode base64 --url)
        ($text | encode base64 --url --nopad)
    ]
}

# A Deployment, StatefulSet or DaemonSet as `kubectl get -o json` gives it, with `spec` as its pod's.
def workload [kind: string, name: string, pod: record] {
    { kind: $kind, metadata: { name: $name }, spec: { template: { spec: $pod } } }
}

# A directory holding a `kubectl` that keeps Secrets as files beside it (`<namespace>-<name>.json`), answers
# `get deployments,statefulsets,daemonsets` from `workloads.json`, and logs every command line to `argv.log`.
# Enough of kubectl for what the secrets step asks of it, with no cluster.
def fake_cluster [] {
    let dir = (mktemp --directory)
    let script = '
def --wrapped main [...args] {
    let dir = ($env.CURRENT_FILE | path dirname)
    $"($args | to json --raw)\n" | save --append ($dir | path join "argv.log")
    let at = ($args | enumerate | where item == "-n" | get -o 0.index)
    let ns = if $at == null { null } else { $args | get ($at + 1) }
    match [($args | get 0) ($args | get 1)] {
        ["get" "secret"] => {
            let f = ($dir | path join $"($ns)-($args | get 2).json")
            if ($f | path exists) { print (open --raw $f) } else { print -e "NotFound"; exit 1 }
        }
        ["create" "secret"] => {
            let name = ($args | get 3)
            let data = ($args | window 2 | where {|w| ($w | get 0) == "--from-literal" } | each {|w| $w | get 1 | parse --regex "^(?<k>[^=]+)=(?<v>.*)$" | first } | reduce --fold {} {|kv, acc| $acc | upsert $kv.k ($kv.v | encode base64) })
            { apiVersion: "v1", kind: "Secret", type: "Opaque", metadata: { name: $name, namespace: $ns }, data: $data } | to json | save --force ($dir | path join $"($ns)-($name).json")
            print $"secret/($name) created"
        }
        ["replace" "-f"] => {
            let s = (^cat | from json)
            $s | to json | save --force ($dir | path join $"($s.metadata.namespace)-($s.metadata.name).json")
            print $"secret/($s.metadata.name) replaced"
        }
        ["get" "deployments,statefulsets,daemonsets"] => {
            let f = ($dir | path join "workloads.json")
            print (if ($f | path exists) { open --raw $f } else { "{\"items\": []}" })
        }
        _ => { print -e $"fake kubectl: ($args | str join (char space))"; exit 2 }
    }
}
'
    $"#!($nu.current-exe)\n($script)" | save ($dir | path join "kubectl")
    ^chmod +x ($dir | path join "kubectl")
    $dir
}

# Puts a Secret in the fake cluster, its data given as plain text.
def fake_secret [dir: string, namespace: string, name: string, data: record, extra: record = {}] {
    let encoded = ($data | items {|k, v| { k: $k, v: ($v | encode base64) } } | reduce --fold {} {|kv, acc| $acc | upsert $kv.k $kv.v })
    { apiVersion: "v1", kind: "Secret", type: "Opaque", metadata: { name: $name, namespace: $namespace }, data: $encoded }
    | merge deep $extra
    | to json
    | save --force ($dir | path join $"($namespace)-($name).json")
}

# Runs `call` (a line of Nu using secrets-lib.nu) against the fake cluster in `dir`; returns `complete`'s record.
def against [dir: string, call: string] {
    let lib = ($env.FILE_PWD | path join ".." "secrets-lib.nu")
    with-env { PATH: ($env.PATH | prepend $dir) } {
        do { ^$nu.current-exe --no-config-file -c $"use '($lib)' *; ($call)" } | complete
    }
}

# The fake cluster's Secret, as kubectl would give it, and its data decoded.
def stored [dir: string, namespace: string, name: string] {
    let s = (open ($dir | path join $"($namespace)-($name).json"))
    $s | insert plain (cluster_secret_data $s)
}

const REQUIRED = [
    { namespace: "polar-graph", name: "neo4j-secret", key: "neo4j_auth" }
    { namespace: "polar", name: "polar-graph-pw", key: "graph_password" }
]

const BOTH_THERE = ["polar-graph/neo4j-secret" "polar/polar-graph-pw"]

def failure [file: string, reason: string] { { file: $file, reason: $reason } }

# A target directory whose shared SOPS file is not a SOPS file, so it cannot be decrypted by anyone.
def broken_target [] {
    let dir = (mktemp --directory)
    "creation_rules: []\n" | save --force ($dir | path join ".sops.yaml")
    "neo4j_auth: not-encrypted\n" | save --force ($dir | path join "secrets.sops.yaml")
    $dir
}

def main [] {
    let tests = [
        {
            name: "a file is decrypted with no .sops.yaml beside it, because it knows its own recipients"
            body: {
                let dir = (mktemp -d)
                let key = ($dir | path join "k")
                ^ssh-keygen -q -t ed25519 -N "" -f $key
                let pub = (open --raw $"($key).pub" | split row " " | first 2 | str join " ")
                $"creation_rules:\n  - path_regex: secrets\\.sops\\.yaml$\n    age: ($pub)\n" | save ($dir | path join ".sops.yaml")
                "neo4j_auth: neo4j/x\n" | save ($dir | path join "secrets.sops.yaml")
                with-env { SOPS_CONFIG: ($dir | path join ".sops.yaml") } { ^sops --encrypt --in-place ($dir | path join "secrets.sops.yaml") }
                rm ($dir | path join ".sops.yaml")
                let got = (with-env { SOPS_AGE_SSH_PRIVATE_KEY_FILE: $key } { decrypt_sops $dir "secrets.sops.yaml" })
                rm -rf $dir
                assert equal $got.failure null
                assert equal $got.data.neo4j_auth "neo4j/x"
            }
        }
        {
            name: "a generated token is long, made of letters and digits, and different each time"
            body: {
                let a = (random_token)
                let b = (random_token)
                assert equal ($a | str length) 48
                assert ($a =~ '^[A-Za-z0-9]+$')
                assert ($a != $b)
                assert equal (random_token 16 | str length) 16
            }
        }
        {
            name: "a target whose SOPS file will not decrypt is told so by the check, with the way out, before anything is built"
            body: {
                let dir = (broken_target)
                let found = (secrets_check $dir [])
                assert equal ($found.problems | length) 2
                for p in $found.problems {
                    assert ($p | str contains "would not decrypt") $p
                    assert ($p | str contains "SOPS_AGE_SSH_PRIVATE_KEY_FILE") $p
                    assert ($p | str contains "SOPS_AGE_BINARY") $p
                }
                assert ($found.failures | any {|f| $f.file == "secrets.sops.yaml" })
                rm -rf $dir
            }
        }
        {
            name: "the check lets a resume through when the secrets already exist, even though the file will not decrypt"
            body: {
                let dir = (broken_target)
                let found = (secrets_check $dir $BOTH_THERE)
                assert equal $found.problems []
                assert ($found.failures | is-not-empty)
                rm -rf $dir
            }
        }
        {
            name: "the check names what is missing when there is nothing to decrypt and nothing exists"
            body: {
                let dir = (mktemp --directory)
                let found = (secrets_check $dir [])
                assert equal ($found.problems | length) 2
                assert ($found.problems | all {|p| $p | str contains "is not in the decrypted files" })
                rm -rf $dir
            }
        }
        {
            name: "the secrets every target needs are written once"
            body: {
                assert equal ($REQUIRED_SECRETS | get name | sort) ["neo4j-secret" "polar-graph-pw"]
            }
        }
        {
            name: "a decrypt failure is harmless when every required secret already exists"
            body: {
                let problems = (required_secret_problems $REQUIRED $BOTH_THERE {} [(failure "secrets.sops.yaml" "no identity matched")])
                assert equal $problems []
            }
        }
        {
            name: "a missing secret that could not be decrypted is a problem that says what to check"
            body: {
                let problems = (required_secret_problems $REQUIRED ["polar/polar-graph-pw"] {} [(failure "secrets.sops.yaml" "no identity matched")])
                assert equal ($problems | length) 1
                let p = ($problems | first)
                assert ($p | str contains "polar-graph/neo4j-secret")
                assert ($p | str contains "secrets.sops.yaml")
                assert ($p | str contains "no identity matched")
                assert ($p | str contains "SOPS_AGE_SSH_PRIVATE_KEY_FILE")
            }
        }
        {
            name: "a missing secret whose value was decrypted is no problem, it will be created"
            body: {
                let problems = (required_secret_problems $REQUIRED [] { neo4j_auth: "neo4j/x", graph_password: "x" } [])
                assert equal $problems []
            }
        }
        {
            name: "a missing secret with no value in files that decrypted fine names the key, not the keys to SOPS"
            body: {
                let problems = (required_secret_problems $REQUIRED ["polar-graph/neo4j-secret"] { other: "y" } [])
                assert equal ($problems | length) 1
                let p = ($problems | first)
                assert ($p | str contains "graph_password")
                assert (not ($p | str contains "SOPS_AGE"))
            }
        }
        {
            name: "every missing secret is reported, not only the first"
            body: {
                let problems = (required_secret_problems $REQUIRED [] {} [(failure "user.sops.yaml" "boom")])
                assert equal ($problems | length) 2
            }
        }
        {
            name: "an empty value counts as no value"
            body: {
                let problems = (required_secret_problems $REQUIRED ["polar/polar-graph-pw"] { neo4j_auth: "" } [])
                assert equal ($problems | length) 1
            }
        }
        {
            name: "the first line of a long error is what is shown"
            body: {
                let line = (first_line "Group 0: FAILED\n  ssh-rsa AAAA...\n  more")
                assert equal $line "Group 0: FAILED"
            }
        }

        # ── a Secret that already exists ──────────────────────────────────────
        {
            name: "a Secret the cluster does not have is created, of either kind"
            body: {
                assert equal (secret_action "sops" null { token: "fake-1" }) "create"
                assert equal (secret_action "sops" null { token: "placeholder" } --placeholder) "create"
                assert equal (secret_action "generated" null { token: "fake-1" }) "create"
            }
        }
        {
            name: "a Secret from the SOPS files that holds what they say is unchanged, whatever the order of its keys"
            body: {
                assert equal (secret_action "sops" { token: "fake-1" } { token: "fake-1" }) "unchanged"
                assert equal (secret_action "sops" { username: "u", password: "p" } { password: "p", username: "u" }) "unchanged"
            }
        }
        {
            name: "a Secret from the SOPS files that holds something else is updated"
            body: {
                assert equal (secret_action "sops" { token: "fake-old" } { token: "fake-new" }) "update"
                assert equal (secret_action "sops" { username: "u", password: "p" } { username: "u", password: "q" }) "update"
            }
        }
        {
            name: "a Secret with a key the SOPS files do not give, or without one they do, is updated"
            body: {
                assert equal (secret_action "sops" { token: "fake-1", extra: "x" } { token: "fake-1" }) "update"
                assert equal (secret_action "sops" { username: "u" } { username: "u", password: "p" }) "update"
            }
        }
        {
            name: "a placeholder never overwrites a value the cluster holds: the SOPS files have none, so the cluster's is kept"
            body: {
                assert equal (secret_action "sops" { token: "fake-set-by-hand" } { token: "placeholder" } --placeholder) "keep-cluster"
                assert equal (secret_action "sops" { oci-registry-auth: '{"auths":{}}' } { oci-registry-auth: "{}" } --placeholder) "keep-cluster"
            }
        }
        {
            name: "a placeholder the cluster already holds is unchanged"
            body: {
                assert equal (secret_action "sops" { token: "placeholder" } { token: "placeholder" } --placeholder) "unchanged"
            }
        }
        {
            name: "a value in the SOPS files replaces the placeholder the cluster holds"
            body: {
                assert equal (secret_action "sops" { token: "placeholder" } { token: "fake-real" }) "update"
            }
        }
        {
            name: "a generated Secret that exists is never compared, so never replaced"
            body: {
                assert equal (secret_action "generated" { token: "fake-old" } { token: "fake-new" }) "unchanged"
                assert equal (secret_action "generated" {} { token: "fake-new" }) "unchanged"
            }
        }
        {
            name: "a Secret must say which kind it is"
            body: {
                assert error {|| secret_action "other" null { token: "x" } }
                assert error {|| secret_action "generated" null { token: "x" } --placeholder }
            }
        }
        {
            name: "property: for any data, a Secret from the SOPS files is created when missing, unchanged when equal, and never updated by a placeholder"
            body: {
                for _ in 1..100 {
                    let desired = { (printable 6): (printable 20) }
                    let other = { ($desired | columns | first): (printable 21) }
                    assert equal (secret_action "sops" null $desired) "create"
                    assert equal (secret_action "sops" $desired $desired) "unchanged"
                    assert equal (secret_action "sops" $other $desired) "update"
                    assert equal (secret_action "sops" $other $desired --placeholder) "keep-cluster"
                    assert equal (secret_action "generated" $other $desired) "unchanged"
                }
            }
        }
        {
            name: "literals become data split at the first =, so a value may hold one"
            body: {
                assert equal (literal_data ["token=fake=1==" "user=u"]) { token: "fake=1==", user: "u" }
                assert equal (literal_data ["oci-registry-auth={}"]) { oci-registry-auth: "{}" }
            }
        }
        {
            name: "the cluster's data is decoded from base64, and a Secret that is not there is null"
            body: {
                let got = { data: { token: ("fake-1" | encode base64), empty: "" } }
                assert equal (cluster_secret_data $got) { token: "fake-1", empty: "" }
                assert equal (cluster_secret_data { metadata: {} }) {}
                assert equal (cluster_secret_data null) null
            }
        }
        {
            name: "the workloads that read a Secret are found in env, envFrom, volumes and projected volumes, of init containers too"
            body: {
                let items = [
                    (workload "Deployment" "by-env" { containers: [ { env: [ { name: "T", valueFrom: { secretKeyRef: { name: "jira-secret", key: "token" } } } ] } ] })
                    (workload "StatefulSet" "by-volume" { containers: [ {} ], volumes: [ { name: "v", secret: { secretName: "jira-secret" } } ] })
                    (workload "DaemonSet" "by-env-from" { containers: [ { envFrom: [ { secretRef: { name: "jira-secret" } } ] } ] })
                    (workload "Deployment" "by-projected" { containers: [ {} ], volumes: [ { name: "v", projected: { sources: [ { secret: { name: "jira-secret" } } ] } } ] })
                    (workload "Deployment" "by-init" { initContainers: [ { env: [ { name: "T", valueFrom: { secretKeyRef: { name: "jira-secret", key: "token" } } } ] } ], containers: [ {} ] })
                    (workload "Deployment" "other-secret" { containers: [ { env: [ { name: "T", valueFrom: { secretKeyRef: { name: "gitlab-secret", key: "token" } } } ] } ] })
                    (workload "Deployment" "a-configmap" { containers: [ { env: [ { name: "T", valueFrom: { configMapKeyRef: { name: "jira-secret", key: "token" } } } ] } ] })
                    (workload "Deployment" "plain" { containers: [ { env: [ { name: "X", value: "y" } ] } ] })
                ]
                assert equal (secret_readers $items "jira-secret") [
                    "daemonset/by-env-from" "deployment/by-env" "deployment/by-init" "deployment/by-projected" "statefulset/by-volume"
                ]
                assert equal (secret_readers $items "keycloak-db-secret") []
            }
        }
        {
            name: "an updated Secret names the command that restarts what reads it, and a generic hint when nothing is known"
            body: {
                let line = (secret_line "sops" "update" "jira-secret" "polar" ["deployment/jira-observer"])
                assert equal $line "    secret/jira-secret in polar: updated from the sops files (restart the pods that read it: kubectl -n polar rollout restart deployment/jira-observer)"
                let two = (secret_line "sops" "update" "polar-graph-pw" "polar" ["deployment/a" "statefulset/b"])
                assert ($two | str contains "kubectl -n polar rollout restart deployment/a statefulset/b")
                let none = (secret_line "sops" "update" "x" "polar" [])
                assert ($none | str contains "restart the pods that read it")
                assert (not ($none | str contains "kubectl"))
            }
        }
        {
            name: "every outcome is one line that says what happened, and none holds a value"
            body: {
                let lines = [
                    (secret_line "sops" "create" "jira-secret" "polar" [])
                    (secret_line "sops" "unchanged" "jira-secret" "polar" [])
                    (secret_line "sops" "keep-cluster" "jira-secret" "polar" [])
                    (secret_line "generated" "create" "cassini-shutdown-token" "polar" [])
                    (secret_line "generated" "unchanged" "cassini-shutdown-token" "polar" [])
                ]
                assert equal ($lines | get 0) "    secret/jira-secret created in polar"
                assert equal ($lines | get 1) "    secret/jira-secret in polar: unchanged"
                assert ($lines | get 2 | str contains "no value") ($lines | get 2)
                assert ($lines | get 2 | str contains "the cluster's is kept") ($lines | get 2)
                assert equal ($lines | get 3) "    secret/cassini-shutdown-token created in polar"
                assert ($lines | get 4 | str contains "already exists in polar") ($lines | get 4)
                assert ($lines | all {|l| ($l | lines | length) == 1 })
            }
        }

        {
            name: "a Secret that is missing is created from the SOPS files, and a second run finds it unchanged"
            body: {
                let dir = (fake_cluster)
                let first = (against $dir 'kubectl_sops_secret "polar-graph-pw" "polar" ["secret=fake-password"]')
                assert equal $first.exit_code 0 $first.stderr
                assert equal ($first.stdout | str trim) "secret/polar-graph-pw created in polar"
                assert equal (stored $dir "polar" "polar-graph-pw" | get plain) { secret: "fake-password" }
                let second = (against $dir 'kubectl_sops_secret "polar-graph-pw" "polar" ["secret=fake-password"]')
                assert equal ($second.stdout | str trim) "secret/polar-graph-pw in polar: unchanged"
                rm -rf $dir
            }
        }
        {
            name: "a token changed in the SOPS files replaces the placeholder, keeps the rest of the Secret, and names what to restart"
            body: {
                let dir = (fake_cluster)
                fake_secret $dir "polar" "jira-secret" { token: "placeholder" } { metadata: { labels: { keep: "me" }, annotations: { note: "kept" } } }
                {
                    items: [
                        (workload "Deployment" "jira-observer" { containers: [ { env: [ { name: "JIRA_TOKEN", valueFrom: { secretKeyRef: { name: "jira-secret", key: "token" } } } ] } ] })
                        (workload "Deployment" "jira-processor" { containers: [ {} ] })
                    ]
                } | to json | save ($dir | path join "workloads.json")
                let r = (against $dir 'kubectl_sops_secret "jira-secret" "polar" ["token=fake-new-token"]')
                assert equal $r.exit_code 0 $r.stderr
                assert equal ($r.stdout | str trim) "secret/jira-secret in polar: updated from the sops files (restart the pods that read it: kubectl -n polar rollout restart deployment/jira-observer)"
                let s = (stored $dir "polar" "jira-secret")
                assert equal $s.plain { token: "fake-new-token" }
                assert equal $s.type "Opaque"
                assert equal $s.metadata.labels { keep: "me" }
                assert equal $s.metadata.annotations { note: "kept" }
                # Never printed, and never on the command line of the update.
                assert (not ($r.stdout + $r.stderr | str contains "fake-new-token"))
                let replace = (open --raw ($dir | path join "argv.log") | lines | where {|l| $l | str contains "replace" })
                assert equal ($replace | length) 1
                assert (not ($replace | first | str contains "fake-new-token"))
                assert (not ($replace | first | str contains ("fake-new-token" | encode base64)))
                rm -rf $dir
            }
        }
        {
            name: "a placeholder leaves a value set by hand where it is"
            body: {
                let dir = (fake_cluster)
                fake_secret $dir "polar" "oci-registry-auth" { oci-registry-auth: '{"auths":{"fake.example":{}}}' }
                let r = (against $dir 'kubectl_sops_secret "oci-registry-auth" "polar" ["oci-registry-auth={}"] --placeholder')
                assert equal $r.exit_code 0 $r.stderr
                assert ($r.stdout | str contains "the cluster's is kept") $r.stdout
                assert equal (stored $dir "polar" "oci-registry-auth" | get plain) { oci-registry-auth: '{"auths":{"fake.example":{}}}' }
                rm -rf $dir
            }
        }
        {
            name: "a generated Secret that exists is left as it is"
            body: {
                let dir = (fake_cluster)
                fake_secret $dir "polar" "cassini-shutdown-token" { token: "fake-first-token" }
                let r = (against $dir 'kubectl_generated_secret "cassini-shutdown-token" "polar" [$"token=(random_token)"]')
                assert equal $r.exit_code 0 $r.stderr
                assert ($r.stdout | str contains "already exists in polar") $r.stdout
                assert equal (stored $dir "polar" "cassini-shutdown-token" | get plain) { token: "fake-first-token" }
                rm -rf $dir
            }
        }

        # ── values that look base64-encoded ───────────────────────────────────
        {
            name: "a token stored base64-encoded is flagged, in any of the four spellings"
            body: {
                for s in (spellings "fake-jira-token-0123456789") {
                    assert (looks_base64_encoded $s) $s
                }
                # Printable text whose base64 holds + and /, so the URL-safe spelling differs.
                for s in (spellings "~~~~~~~~????????>>>>") {
                    assert (looks_base64_encoded $s) $s
                }
            }
        }
        {
            name: "base64 of short text, of text with whitespace, and what is not base64 at all are not flagged"
            body: {
                assert (not (looks_base64_encoded ("fake-short" | encode base64)))
                assert (not (looks_base64_encoded ("fake token with spaces" | encode base64)))
                assert (not (looks_base64_encoded ("fake-token-line\n" | encode base64)))
                assert (not (looks_base64_encoded "neo4j/fake-password"))
                assert (not (looks_base64_encoded '{"auths":{}}'))
                assert (not (looks_base64_encoded "https://jira.example.com"))
                assert (not (looks_base64_encoded ""))
                # A raw Atlassian Cloud token: = in the middle, so not base64.
                assert (not (looks_base64_encoded "ATATT3xFfGF0fakefakefakefake=0A1B2C3D"))
            }
        }
        {
            name: "property: base64 of any printable text of 16 or more is flagged"
            body: {
                for _ in 1..200 {
                    let text = (printable (random int 16..60))
                    for s in (spellings $text) {
                        assert (looks_base64_encoded $s) $"length ($text | str length)"
                    }
                }
            }
        }
        {
            name: "property: a Jira Data Center token (base64 of 33 random bytes) is not flagged"
            body: {
                for _ in 1..200 {
                    let token = (random binary 33 | encode base64)
                    assert equal ($token | str length) 44
                    assert (not (looks_base64_encoded $token))
                }
            }
        }
        {
            name: "property: generated tokens and hex passwords are not flagged"
            body: {
                for _ in 1..200 {
                    assert (not (looks_base64_encoded (random_token)))
                    assert (not (looks_base64_encoded (random binary 16 | encode hex --lower)))
                }
            }
        }
        {
            name: "a warning names the key and the file the value came from, never the value"
            body: {
                let encoded = ("fake-jira-token-0123456789" | encode base64)
                let warnings = (base64_warnings { jira_token: "fake-raw", graph_password: $encoded } { jira_token: $encoded })
                assert equal ($warnings | length) 2
                assert equal ($warnings | get 0) "graph_password in secrets.sops.yaml looks base64-encoded: store the raw value; Kubernetes encodes Secret values itself"
                assert equal ($warnings | get 1) "jira_token in user.sops.yaml looks base64-encoded: store the raw value; Kubernetes encodes Secret values itself"
                assert ($warnings | all {|w| not ($w | str contains $encoded) })
            }
        }
        {
            name: "a value the user file overrides is not warned about, nor what is not a string"
            body: {
                let encoded = ("fake-jira-token-0123456789" | encode base64)
                assert equal (base64_warnings { jira_token: $encoded } { jira_token: "fake-raw" }) []
                assert equal (base64_warnings { port: 7687, nested: { a: $encoded } } {}) []
            }
        }
        {
            name: "the check returns the warnings with the problems"
            body: {
                let dir = (mktemp --directory)
                let found = (secrets_check $dir [])
                assert equal $found.warnings []
                rm -rf $dir
            }
        }
    ]

    let results = ($tests | each {|t|
        try {
            do $t.body
            { name: $t.name, error: null }
        } catch {|e|
            { name: $t.name, error: $e.msg }
        }
    })
    for r in $results {
        if $r.error == null {
            print $"  ok    ($r.name)"
        } else {
            print -e $"  FAIL  ($r.name): ($r.error)"
        }
    }
    let failed = ($results | where error != null | length)
    if $failed > 0 {
        print -e $"($failed) of ($results | length) failed"
        exit 1
    }
    print $"all ($results | length) passed"
}
