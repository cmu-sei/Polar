# infra/secrets-lib.nu
#
# What the targets' `secrets.nu` scripts share: creating Kubernetes secrets and ConfigMaps that
# are not there yet, decrypting the SOPS files, and deciding whether the step can go on when a
# file will not decrypt. The decision is pure, so it is tested (infra/tests/secrets.nu).
#
# A decrypt that fails is not an error in itself: on a re-run every secret already exists, and
# nothing is needed from the files. It is an error when a secret the cluster needs is missing
# and the files that would have made it could not be read.

export def kubectl_upsert_secret [
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
    if not ($sops_config | path exists) {
        return { data: {}, failure: { file: $file, reason: $"no SOPS config at ($sops_config)" } }
    }

    let result = (
        do {
            with-env { SOPS_CONFIG: $sops_config } {
                run-external "sops" "--decrypt" "--output-type" "json" $path
            }
        } | complete
    )
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

# What would stop the secrets step for a target, found without creating anything: decrypts its SOPS files and
# compares what they hold with what the cluster already has. Returns `{ secrets, failures, problems }`;
# `problems` is empty when the step can go on. `up.nu` asks this first, so a shell without the SOPS variables
# is told so before the images are built and not after, and each target's `secrets.nu` asks it again.
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
    }
}
