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

# infra/secrets-lib.nu
#
# What the targets' `secrets.nu` scripts share: creating Kubernetes secrets and ConfigMaps,
# keeping the Secrets made from the SOPS files equal to them, decrypting the files, and deciding
# whether the step can go on when a file will not decrypt. The decisions are pure, so they are
# tested (infra/tests/secrets.nu); kubectl is called only by the functions that act on them.
#
# A decrypt that fails is not an error in itself: on a re-run every secret already exists, and
# nothing is needed from the files. It is an error when a secret the cluster needs is missing
# and the files that would have made it could not be read.
#
# A Secret is one of two kinds, and each call says which. One from the SOPS files
# (`kubectl_sops_secret`) is made when it is missing and updated when the files say something
# else, so a value changed in the files reaches the cluster on the next run. One made here
# (`kubectl_generated_secret`, a random token) is made once and never compared: making it again
# would change it under whoever already holds it.

# What to do with a Secret: `create` when the cluster has none, `unchanged` when there is nothing to
# do, `update` when its data is not what the SOPS files give, and `keep-cluster` when the files have
# no value for it (`desired` is a `placeholder`) and the cluster holds one of its own, which may have
# been set by hand. `kind` is `sops` or `generated`; a generated Secret that exists is never compared.
# `current` is the cluster's data, decoded, or null when it has no such Secret.
export def secret_action [kind: string, current: any, desired: record, --placeholder] {
    if not ($kind in ["sops" "generated"]) {
        error make { msg: $"a Secret is from the sops files or generated, not ($kind)" }
    }
    if $kind == "generated" and $placeholder {
        error make { msg: "a generated Secret has no placeholder" }
    }
    if $current == null { return "create" }
    if $kind == "generated" or $current == $desired { return "unchanged" }
    if $placeholder { "keep-cluster" } else { "update" }
}

# The data that `kubectl create secret --from-literal` makes of `key=value` literals: split at the
# first `=`, so a value may hold one.
export def literal_data [literals: list<string>] {
    $literals
    | each {|l| $l | parse --regex '^(?<key>[^=]+)=(?<value>.*)$' | first }
    | reduce --fold {} {|kv, acc| $acc | upsert $kv.key $kv.value }
}

# The data of a Secret as `kubectl get secret -o json` gives it, decoded; null when there is none.
export def cluster_secret_data [secret: any] {
    if $secret == null { return null }
    $secret
    | get -o data
    | default {}
    | items {|k, v| { key: $k, value: ($v | decode base64 | decode utf-8) } }
    | reduce --fold {} {|kv, acc| $acc | upsert $kv.key $kv.value }
}

# The workloads among `workloads` (`kubectl get deployments,statefulsets,daemonsets -o json`'s items,
# all in the Secret's namespace) whose pods read the Secret `name`, as `kind/name`, sorted. They read
# it when their pods start (an env var never changes in a running pod), so they are what to restart.
export def secret_readers [workloads: list, name: string] {
    $workloads
    | where {|d|
        let pod = ($d | get -o spec.template.spec | default {})
        let containers = (($pod | get -o containers | default []) ++ ($pod | get -o initContainers | default []))
        let from_env = ($containers | each {|c| $c | get -o env | default [] | each {|e| $e | get -o valueFrom.secretKeyRef.name } } | flatten)
        let from_env_from = ($containers | each {|c| $c | get -o envFrom | default [] | each {|e| $e | get -o secretRef.name } } | flatten)
        let from_volumes = ($pod | get -o volumes | default [] | each {|v|
            [ ($v | get -o secret.secretName) ] ++ ($v | get -o projected.sources | default [] | each {|s| $s | get -o secret.name })
        } | flatten)
        $name in ($from_env ++ $from_env_from ++ $from_volumes)
    }
    | each {|d| $"($d.kind | str lowercase)/($d.metadata.name)" }
    | sort
}

# The line the secrets step prints for a Secret, given what was done with it. It never holds a value.
export def secret_line [kind: string, action: string, name: string, namespace: string, readers: list] {
    let id = $"secret/($name)"
    match $action {
        "create" => $"    ($id) created in ($namespace)"
        "unchanged" => (if $kind == "generated" {
            $"    ($id) already exists in ($namespace); made once, never replaced"
        } else {
            $"    ($id) in ($namespace): unchanged"
        })
        "keep-cluster" => $"    ($id) in ($namespace): the sops files have no value for it; the cluster's is kept"
        "update" => {
            let how = if ($readers | is-empty) { "" } else { $": kubectl -n ($namespace) rollout restart ($readers | str join ' ')" }
            $"    ($id) in ($namespace): updated from the sops files \(restart the pods that read it($how)\)"
        }
    }
}

# The cluster's data of the Secret `name`, decoded, or null when it has none.
def current_secret_data [name: string, namespace: string] {
    let got = (do { ^kubectl get secret $name -n $namespace -o json } | complete)
    if $got.exit_code != 0 { return null }
    cluster_secret_data ($got.stdout | from json)
}

# Makes or updates the Secret `name` as `secret_action` decides, and says what it did.
def sync_secret [kind: string, name: string, namespace: string, literals: list<string>, placeholder: bool] {
    let desired = (literal_data $literals)
    let action = (secret_action $kind (current_secret_data $name $namespace) $desired --placeholder=$placeholder)
    match $action {
        "create" => {
            let args = ($literals | each {|l| ["--from-literal" $l] } | flatten)
            let created = (do { ^kubectl create secret generic $name -n $namespace ...$args } | complete)
            if $created.exit_code != 0 {
                error make { msg: $"could not create secret/($name) in ($namespace): (first_line $created.stderr)" }
            }
        }
        "update" => {
            # Its data replaced, the rest of it (type, labels, annotations) as it is. Through stdin, so no value is
            # on a command line.
            let secret = (^kubectl get secret $name -n $namespace -o json | from json)
            let data = ($desired | items {|k, v| { key: $k, value: ($v | encode base64) } } | reduce --fold {} {|kv, acc| $acc | upsert $kv.key $kv.value })
            let replaced = ($secret | reject -o stringData | upsert data $data | to json | do { ^kubectl replace -f - } | complete)
            if $replaced.exit_code != 0 {
                error make { msg: $"could not update secret/($name) in ($namespace): (first_line $replaced.stderr)" }
            }
        }
    }
    let readers = if $action == "update" {
        let got = (do { ^kubectl get deployments,statefulsets,daemonsets -n $namespace -o json } | complete)
        if $got.exit_code == 0 { secret_readers ($got.stdout | from json | get items) $name } else { [] }
    } else { [] }
    print (secret_line $kind $action $name $namespace $readers)
}

# A Secret whose values come from the SOPS files: made when missing, updated when its data is not
# what they give. `--placeholder` says the files have no value for it and `literals` hold a stand-in
# (`token=placeholder`), which is made when the Secret is missing and never replaces what the cluster has.
export def kubectl_sops_secret [name: string, namespace: string, literals: list<string>, --placeholder] {
    sync_secret "sops" $name $namespace $literals $placeholder
}

# A Secret whose values are made here (`random_token`): made once, never compared, never made again.
export def kubectl_generated_secret [name: string, namespace: string, literals: list<string>] {
    sync_secret "generated" $name $namespace $literals false
}

export def kubectl_upsert_configmap [
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


# A secret value made here instead of kept in the SOPS files: letters and digits, so it is safe
# in a header or a command line. For a token nothing outside the cluster needs to know in advance;
# whoever does read it from the Secret.
export def random_token [length: int = 48] {
    random chars --length $length
}

# True when the secret `name` exists in `namespace`.
export def secret_exists [name: string, namespace: string] {
    (do { run-external "kubectl" "get" "secret" $name "-n" $namespace } | complete | get exit_code) == 0
}

# The first line of a command's complaint: SOPS prints the keys it tried after it.
export def first_line [text: string] {
    $text | lines | first | default "" | str trim
}

# Decrypts a SOPS file of `target_dir` to a record. Returns `{ data, failure }`: `failure` is null
# when it worked (or when the file is not there at all, which is not a failure to read it), and
# otherwise `{ file, reason }`.
export def decrypt_sops [target_dir: string, file: string] {
    let path = ($target_dir | path join $file)
    if not ($path | path exists) {
        return { data: {}, failure: null }
    }

    let sops_config = if ($file == "user.sops.yaml") {
        ($target_dir | path join ".user.sops.yaml")
    } else {
        ($target_dir | path join ".sops.yaml")
    }

    # The config holds the rules for *encrypting* (who the recipients are). A file already knows its own recipients,
    # so decrypting needs none, and it is gitignored: a fresh checkout has none, and must still be able to read a
    # file its key was added to.
    let result = if ($sops_config | path exists) {
        do {
            with-env { SOPS_CONFIG: $sops_config } {
                run-external "sops" "--decrypt" "--output-type" "json" $path
            }
        } | complete
    } else {
        do { run-external "sops" "--decrypt" "--output-type" "json" $path } | complete
    }
    if $result.exit_code != 0 {
        return { data: {}, failure: { file: $file, reason: (first_line $result.stderr) } }
    }
    { data: ($result.stdout | from json), failure: null }
}

# The problems that stop the secrets step, as sentences; empty when it can go on.
#
# `required` is the secrets the cluster cannot run without, each `{ namespace, name, key }` where
# `key` is the entry of the decrypted files that holds its value. `existing` is the `namespace/name`
# of those already in the cluster. `secrets` is the decrypted values, and `failures` the files that
# could not be decrypted, each `{ file, reason }`.
export def required_secret_problems [required: list, existing: list, secrets: record, failures: list] {
    $required
    | where {|r| not ($"($r.namespace)/($r.name)" in $existing) }
    | where {|r| ($secrets | get -o $r.key | default "" | is-empty) }
    | each {|r|
        let id = $"($r.namespace)/($r.name)"
        if ($failures | is-empty) {
            $"secret ($id) does not exist and `($r.key)` is not in the decrypted files"
        } else {
            let why = ($failures | each {|f| $"($f.file): ($f.reason)" } | str join "; ")
            $"secret ($id) does not exist and could not be made, because the SOPS files would not decrypt \(($why)\). Check SOPS_AGE_SSH_PRIVATE_KEY_FILE and SOPS_AGE_BINARY, which the dev shell sets"
        }
    }
}


# The secrets the cluster cannot run without, each `{ namespace, name, key }` where `key` is the entry of the
# decrypted files that holds its value. Written once: every target needs the same two.
export const REQUIRED_SECRETS = [
    { namespace: "polar-graph", name: "neo4j-secret", key: "neo4j_auth" }
    { namespace: "polar", name: "polar-graph-pw", key: "graph_password" }
]

# The `namespace/name` of the required secrets the cluster already has.
export def existing_required_secrets [] {
    $REQUIRED_SECRETS
    | where {|r| secret_exists $r.name $r.namespace }
    | each {|r| $"($r.namespace)/($r.name)" }
}

# True when `value` looks like base64 of text: it decodes (standard or URL-safe, padded or not) to 16 or more
# printable ASCII characters with no whitespace. Kubernetes encodes a Secret's values itself, so a value stored
# this way reaches the workload still encoded. A real token that is base64 itself (a Jira Data Center token is
# base64 of random bytes) decodes to bytes that are not text, and is not flagged.
export def looks_base64_encoded [value: string] {
    let decodings = [
        { try { $value | decode base64 } catch { null } }
        { try { $value | decode base64 --nopad } catch { null } }
        { try { $value | decode base64 --url } catch { null } }
        { try { $value | decode base64 --url --nopad } catch { null } }
    ]
    $decodings | any {|d|
        let bytes = (do $d)
        # An invalid UTF-8 byte decodes to U+FFFD, which the pattern does not take.
        $bytes != null and (($bytes | decode utf-8) =~ '^[!-~]{16,}$')
    }
}

# A warning for each value of the SOPS files that looks base64-encoded, naming its key and the file it came from
# (`user.sops.yaml` wins over `secrets.sops.yaml`, as in `secrets_check`), never the value. Warned, not refused:
# a value can look encoded and be meant.
export def base64_warnings [shared: record, user: record] {
    let from_shared = ($shared | reject -o ...($user | columns) | items {|k, v| { file: "secrets.sops.yaml", key: $k, value: $v } })
    let from_user = ($user | items {|k, v| { file: "user.sops.yaml", key: $k, value: $v } })
    $from_shared ++ $from_user
    | where {|e| ($e.value | describe) == "string" and (looks_base64_encoded $e.value) }
    | each {|e| $"($e.key) in ($e.file) looks base64-encoded: store the raw value; Kubernetes encodes Secret values itself" }
}

# What would stop the secrets step for a target, found without creating anything: decrypts its SOPS files and
# compares what they hold with what the cluster already has. Returns `{ secrets, failures, problems, warnings }`;
# `problems` is empty when the step can go on, and `warnings` names the values that look base64-encoded. `up.nu`
# asks this first, so a shell without the SOPS variables is told so before the images are built and not after,
# and each target's `secrets.nu` asks it again.
export def secrets_check [target_dir: string, existing: list] {
    let shared = (decrypt_sops $target_dir "secrets.sops.yaml")
    let user   = (decrypt_sops $target_dir "user.sops.yaml")
    let failures = ([$shared.failure $user.failure] | compact)
    # Merge: user values take precedence
    let secrets = ($shared.data | merge $user.data)
    {
        secrets: $secrets
        failures: $failures
        problems: (required_secret_problems $REQUIRED_SECRETS $existing $secrets $failures)
        warnings: (base64_warnings $shared.data $user.data)
    }
}
