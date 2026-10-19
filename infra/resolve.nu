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

# infra/resolve.nu
#
# The resolver: a pure function from configuration to the exact record each
# workload's template needs. It reads infra/config/*.nuon and
# infra/images.nuon, applies the target's overrides, and touches nothing
# else: no cluster, no network, no files written. That makes it testable and
# lets `explain` show what a workload gets and which layer each value came
# from.
#
#   nu infra/resolve.nu list
#   nu infra/resolve.nu explain kube-consumer [--target local-minimal]
#   nu infra/resolve.nu json kube-consumer          # the record the template takes
#   nu infra/resolve.nu check                       # validate every workload
#   nu infra/resolve.nu diff kube-consumer          # compare with the rendered manifest
#
# Layers, lowest precedence first:
#   kind defaults (config/kinds.nuon)
#   workload entry (config/workloads/*.nuon)
#   target override (targets/<target>/config.nuon, under workloads.<name>)
# Capabilities (config/capabilities.nuon) are unioned into lists
# (env, volumes, mounts, certs, dependency checks), in the order declared.

use rules.nu *
use document.nu *
use runtime.nu [container_runtime, entrypoint_for, insecure_pull_flags]

const INFRA = (path self | path dirname)

use cluster-inputs.nu [env_for, env_merged, path_of, read_inputs]
use cache.nu [cached_text, content_key]
use digest.nu [registry_digest]
const CONFIG = ($INFRA | path join "config")

const DEFAULT_TARGET = "local-minimal"

# ── Loading ──────────────────────────────────────────────────────────────────

def target_config [target: string] {
    let p = ($INFRA | path join $"targets/($target)/config.nuon")
    if ($p | path exists) { open $p } else { {} }
}

def load_cluster [target: string] {
    let overrides = (target_config $target | get -o cluster | default {})
    open ($CONFIG | path join "cluster.nuon") | merge deep $overrides
}

# Every workload the catalog defines, whatever a target says.
def catalog_workloads [] {
    glob ($CONFIG | path join "workloads/*.nuon") | sort | each {|f| open $f } | flatten
}

# The workloads a target leaves out: `enabled: false` under workloads.<name> in its config.
def disabled_in [target: string] {
    target_config $target
    | get -o workloads
    | default {}
    | transpose name override
    | where {|w| ($w.override | describe | str starts-with "record") and (($w.override | get -o enabled) == false) }
    | get name
}

# The workloads of the catalog that this target runs.
def all_workloads [target: string] {
    let off = (disabled_in $target)
    catalog_workloads | where {|w| not ($w.name in $off) }
}

# <service> -> { dns, port, addr }, from services.nuon and the cluster domain.
def service_vars [services: record, cluster: record] {
    $services
    | transpose name s
    | reduce --fold {} {|it, acc|
        let ns = ($cluster.namespaces | get $it.s.namespace)
        let dns = $"($it.s.dns_name).($ns).svc.($cluster.domain)"
        let addr = $"($dns):($it.s.port)"
        # A service that says its scheme has a URL; one that does not is not addressed by URL.
        let base = { name: $it.s.dns_name, dns: $dns, port: $it.s.port, addr: $addr }
        $acc | insert $it.name (if ($it.s.scheme? | is-empty) { $base } else { $base | insert url $"($it.s.scheme)://($addr)" })
    }
}

# Replaces {workload} (the workload's own name) and {<service>.dns|port|addr}. A placeholder left over is an error:
# it means a capability names a service that services.nuon does not have.
def subst [text: string, vars: record, who: string] {
    let out = (
        $vars
        | transpose name v
        | reduce --fold ($text | str replace --all "{workload}" $who) {|it, acc|
            $acc
            | str replace --all $"{($it.name).name}" $it.v.name
            | str replace --all $"{($it.name).dns}" $it.v.dns
            | str replace --all $"{($it.name).port}" ($it.v.port | into string)
            | str replace --all $"{($it.name).addr}" $it.v.addr
            | str replace --all $"{($it.name).url}" ($it.v.url? | default $"{($it.name).url}")
        }
    )
    if ($out =~ '\{[A-Za-z0-9-]+\.(name|dns|port|addr|url)\}') {
        error make { msg: $"($who): unresolved placeholder in '($text)'" }
    }
    $out
}

def image_ref [images: list, cluster: record, name: string, who: string] {
    let hit = ($images | where image == $name)
    if ($hit | is-empty) {
        error make { msg: $"($who): image '($name)' has no row in infra/images.nuon" }
    }
    let i = ($hit | first)
    $"($cluster.registry)/($i.image):($i.tag)"
}

def env_entry [e: record, vars: record, from: string, who: string] {
    {
        name: $e.name
        value: (if ($e.value? | is-empty) { null } else { subst $e.value $vars $who })
        secret: (if ($e.secret? | is-empty) { null } else { subst $e.secret $vars $who })
        key: ($e.key? | default null)
        from: $from
    }
}

# A kind with `extends` is that kind with its own fields laid over it: a service is an agent
# that is not a Cassini client, so it says `extends: "agent"` and `needs: []` and nothing else.
def kind_of [kinds: record, name: string] {
    let kind = ($kinds | get $name)
    if ($kind.extends? | is-empty) {
        $kind
    } else {
        (kind_of $kinds $kind.extends) | merge deep ($kind | reject extends)
    }
}

# ── The agent's own scaffold ─────────────────────────────────────────────────

# Where a document agent's pod finds its configuration.
const DOCUMENT_DIR = "/etc/polar"
const DOCUMENT_FILE = "config.json"
const DOCUMENT_VOLUME = "polar-config"

# Where the resolver gets an agent's own answers (`--scaffold`, `--check`):
#   cargo   `cargo run -p <package>`: the source tree, for work on a developer's machine
#   images  the built image in the registry this machine pushes to (POLAR_REGISTRY, default
#           localhost:5000): the exact artifact that deploys. `up.nu` uses this.
def agent_source [] { $env.POLAR_AGENT_SOURCE? | default "cargo" }

def push_registry [] { $env.POLAR_REGISTRY? | default "localhost:5000" }

# A local registry speaks plain HTTP.
def registry_flags [] {
    let r = (push_registry)
    if ($r | str starts-with "localhost") or ($r | str starts-with "127.") { ["--tls-verify=false"] } else { [] }
}

# The image as this machine pulls it from the registry it pushed to (the cluster's name for it is
# different: see `image_ref`).
def pushed_image_ref [images: list, name: string, who: string] {
    let hit = ($images | where image == $name)
    if ($hit | is-empty) {
        error make { msg: $"($who): image '($name)' has no row in infra/images.nuon" }
    }
    let i = ($hit | first)
    $"(push_registry)/($i.image):($i.tag)"
}

# Runs the agent in its image, with no network, asking it `args`. `extra` is more `podman run`
# options (a mounted file, environment). Returns the completed process.
def run_in_image [ref: string, args: list<string>, extra: list<string> = []] {
    # A pull of a tag that has moved leaves the image it replaced behind, untagged, and every
    # `up.nu --dev` moves them all. Remember which it was, and remove it once the new one is here.
    let rt = (container_runtime)
    let before = (^$rt image inspect --format '{{.Id}}' $ref | complete | get stdout | str trim)
    let pulled = (^$rt pull -q ...(insecure_pull_flags $rt (registry_flags)) $ref | complete)
    if $pulled.exit_code != 0 {
        error make { msg: $"cannot pull ($ref): ($pulled.stderr | str trim). Build and push it first: nu infra/images.nu push <image>" }
    }
    let after = (^$rt image inspect --format '{{.Id}}' $ref | str trim)
    if ($before | is-not-empty) and $before != $after {
        # Fails, harmlessly, if something still uses it.
        ^$rt rmi $before | complete | ignore
    }
    let config = (^$rt image inspect --format '{{json .Config}}' $ref | from json)
    let e = (entrypoint_for $rt (image_command $config))
    ^$rt run --rm --network none --entrypoint $e.entrypoint ...$extra $ref ...$e.leading ...$args | complete
}

# The scaffold the agent prints about itself (`<agent> --scaffold`): from the binary, never from a
# copy. `POLAR_SCAFFOLD_DIR` names a directory of `<workload>.json` files to read instead, for a
# check that must not build anything.
def scaffold_of [name: string, spec: record] {
    let dir = ($env.POLAR_SCAFFOLD_DIR? | default "")
    if ($dir | is-not-empty) {
        let file = ($dir | path join $"($name).json")
        if not ($file | path exists) {
            error make { msg: $"($name): no scaffold at ($file)" }
        }
        return (open $file)
    }
    if (agent_source) == "images" {
        let ref = (pushed_image_ref (open ($INFRA | path join "images.nuon")) $spec.image $name)
        # Starting the agent's container to read its scaffold takes about 0.4 s, and a render asks for it a few dozen times. What an
        # image prints for --scaffold is a function of the image, so it is kept under the digest the registry serves for it: a
        # rebuilt image is another key. With no digest to key on (no registry answer) nothing is kept.
        let digest = (registry_digest $ref)
        let compute = {||
            let ran = (try { run_in_image $ref ["--scaffold"] } catch {|e| error make { msg: $"($name): ($e.msg)" } })
            if $ran.exit_code != 0 {
                error make { msg: $"($name): `($ref) --scaffold` failed: ($ran.stderr | lines | last 5 | str join ' | ')" }
            }
            $ran.stdout
        }
        let text = if $digest == null { do $compute } else { cached_text "scaffold" (content_key ["scaffold" $ref $digest]) $compute }
        return ($text | from json)
    }
    let package = ($spec.scaffold_package? | default $spec.image)
    let ran = (do { cd ($INFRA | path join "../src/agents"); ^cargo run -q -p $package -- --scaffold } | complete)
    if $ran.exit_code != 0 {
        error make { msg: $"($name): `cargo run -p ($package) -- --scaffold` failed: ($ran.stderr | lines | last 5 | str join ' | ')" }
    }
    $ran.stdout | from json
}

# The agent's own judgement of the document it will be given: `<agent> --check --config FILE`, the
# function it runs when it starts. Skipped when scaffolds come from a directory, since then there is
# no binary to ask. The secrets the document refers to are given dummy values: only that they are
# set matters here.
# What the agent check needs to name: the image it pulls (a row of images.nuon) and the crate it builds with
# cargo (`-p`). They are one name unless the crate is not named for its image (`scaffold_package`).
def check_names [spec: record] {
    { image: $spec.image, package: ($spec.scaffold_package? | default $spec.image) }
}

def agent_check [name: string, r: record] {
    let dir = ($env.POLAR_SCAFFOLD_DIR? | default "")
    if ($dir | is-not-empty) { return [] }
    let file = (mktemp --suffix ".json")
    $r.config_map.content | save --force $file
    let secrets = ($r.env | where secret != null | get name | reduce --fold {} {|n, acc| $acc | insert $n "x" })
    let ran = if (agent_source) == "images" {
        chmod 644 $file
        let ref = (pushed_image_ref (open ($INFRA | path join "images.nuon")) $r.check_names.image $name)
        let env_flags = ($secrets | columns | each {|n| ["-e" $"($n)=x"] } | flatten)
        try {
            run_in_image $ref ["--check" "--config" "/polar-config.json"] (["-v" $"($file):/polar-config.json:ro,Z"] ++ $env_flags)
        } catch {|e| { exit_code: 1, stderr: $"ERROR: ($e.msg)" } }
    } else {
        let package = $r.check_names.package
        do {
            cd ($INFRA | path join "../src/agents")
            with-env $secrets { ^cargo run -q -p $package -- --check --config $file }
        } | complete
    }
    rm -f $file
    if $ran.exit_code == 0 { [] } else {
        $ran.stderr | lines | where {|l| $l | str starts-with "ERROR:" } | each {|l| $"($name): ($l | str replace 'ERROR: ' '')" }
    }
}

# ── Resolution ───────────────────────────────────────────────────────────────

export def resolve [name: string, target: string = $DEFAULT_TARGET] {
    let cluster = (load_cluster $target)
    let services = (open ($CONFIG | path join "services.nuon"))
    let caps = (open ($CONFIG | path join "capabilities.nuon"))
    let kinds = (open ($CONFIG | path join "kinds.nuon"))
    let images = (open ($INFRA | path join "images.nuon"))
    let vars = (service_vars $services $cluster)

    let found = (catalog_workloads | where name == $name)
    if ($found | is-empty) {
        error make { msg: $"no workload named '($name)'. Known: (catalog_workloads | get name | str join ', ')" }
    }
    if ($name in (disabled_in $target)) {
        error make { msg: $"workload '($name)' is not enabled in target '($target)': its config sets `enabled: false`" }
    }
    let entry = ($found | first)
    let ov = (target_config $target | get -o workloads | default {} | get -o $name | default {})

    if not ($entry.kind in ($kinds | columns)) {
        error make { msg: $"($name): unknown kind '($entry.kind)'" }
    }
    let kind = (kind_of $kinds $entry.kind)

    # Scalars and nested records: kind < entry < target. Lists below.
    let spec = (
        $kind
        | merge deep ($entry | reject -o needs env)
        | merge deep ($ov | reject -o needs env)
    )
    let origin = (
        $spec | columns | reduce --fold {} {|k, acc|
            let o = if ($ov | get -o $k) != null { "target" } else if ($entry | get -o $k) != null { "entry" } else { $"kind:($entry.kind)" }
            $acc | insert $k $o
        }
    )

    # A catalog agent implements PolarAgent: it says what it needs and the least time it needs to
    # drain in its own scaffold, and the catalog supplies what only the deployment knows. An agent
    # that cannot yet say so is a hand-written chart under infra/layers, not a catalog workload.
    if ($spec.config? | default "") != "document" {
        error make { msg: $"($name): a catalog workload is an agent that describes itself and reads a configuration document \(config: \"document\"\). One that does not is a chart under infra/layers" }
    }
    let scaffold = (scaffold_of $name $spec)
    if (($entry.needs? | default [] | is-not-empty) or ($ov.needs? | default [] | is-not-empty)) {
        error make { msg: $"($name): the catalog lists `needs`, but this agent says its own: remove it from the entry \(the binary requires: (requires_of $scaffold | str join ', ')\)" }
    }
    # What the deployment gives beyond what the code requires: a workload may be given the graph
    # without needing it to be healthy, for example. The agent says what it requires in its
    # scaffold and the entry says what else it is provided.
    let provided = (($entry.provides? | default []) ++ ($ov.provides? | default []))
    let needs = ($kind.needs ++ (requires_of $scaffold) ++ $provided | uniq)

    # Services whose certificates the health check still watches but that polar-init does not wait
    # for before the workload starts: one that must be able to start, and serve, while they are down.
    let no_start_wait = (($entry.no_start_wait? | default []) ++ ($ov.no_start_wait? | default []))

    mut env_list: list<any> = []
    mut volumes: list<any> = [
        { name: $spec.sa_token.volume, secret: null, audience: $cluster.sa_token_audience, from: "kind" }
    ]
    mut mounts: list<any> = []
    mut init_mounts: list<any> = []
    mut deps: list<any> = []
    mut certs: list<any> = []
    mut cert_dirs: list<any> = []
    mut sa_secret: any = null
    mut cluster_roles: list<any> = []
    mut pvcs: list<any> = []
    mut init_env: list<any> = []
    mut ports: list<any> = []
    mut exposed: list<any> = []

    for n in $needs {
        if not ($n in ($caps | columns)) {
            error make { msg: $"($name): unknown need '($n)'. Known: ($caps | columns | str join ', ')" }
        }
        let cap = ($caps | get $n)

        # dependency: the service whose certificate must be valid first.
        let dep = $cap.dependency
        if ($dep.service? | is-not-empty) {
            let svc = ($vars | get -o $dep.service)
            if $svc == null {
                error make { msg: $"capability '($n)' names service '($dep.service)', which is not in services.nuon" }
            }
            $deps = ($deps | append { endpoint: $"($svc.addr):($dep.ttl_secs)", service: $dep.service, from: $n })
        }

        # identity: client certificates, each with its volume and mounts.
        for c in ($cap.identity.certs? | default []) {
            $volumes = ($volumes | append { name: $c.volume, secret: null, audience: null, from: $n })
            $mounts = ($mounts | append { name: $c.volume, path: $c.dir, read_only: $c.read_only, from: $n })
            $init_mounts = ($init_mounts | append { name: $c.volume, path: $c.dir, from: $n })
            let cert_kind = ($c.kind? | default "client")
            let san = (if ($c.san? | is-empty) { "" } else { subst $c.san $vars $name })
            $certs = ($certs | append { spec: $"($cert_kind):($c.dir):ecdsa-p256:($san)", from: $n })
            # The files the health check watches for expiry: the certificate, and whatever else the
            # capability says (the broker watches the CA's as well).
            for file in ($c.watch? | default ["cert.pem"]) {
                $cert_dirs = ($cert_dirs | append $"($c.dir)/($file)")
            }
        }

        # init: what polar-init gets besides the certificates it issues.
        for e in ($cap.init.env? | default []) {
            $init_env = ($init_env | append (env_entry $e $vars $n $name))
        }
        for m in ($cap.init.mounts? | default []) {
            $init_mounts = ($init_mounts | append { name: (subst $m.name $vars $name), path: (subst $m.path $vars $name), from: $n })
        }

        # pod: what the agent container gets.
        for e in ($cap.pod.env? | default []) {
            $env_list = ($env_list | append (env_entry $e $vars $n $name))
        }
        for v in ($cap.pod.volumes? | default []) {
            let sec = (if ($v.secret? | is-empty) { null } else { subst $v.secret $vars $name })
            let claim = (if ($v.claim? | is-empty) { null } else { subst $v.claim $vars $name })
            let cm = (if ($v.config_map? | is-empty) { null } else { subst $v.config_map $vars $name })
            let items = ($v.items? | default [])
            $volumes = ($volumes | append { name: (subst $v.name $vars $name), secret: $sec, audience: null, claim: $claim, config_map: $cm, items: $items, from: $n })
        }
        for m in ($cap.pod.mounts? | default []) {
            $mounts = ($mounts | append { name: (subst $m.name $vars $name), path: (subst $m.path $vars $name), read_only: $m.read_only, from: $n })
        }

        # resources: objects that live alongside the workload.
        if ($cap.resources.sa_token_secret? | is-not-empty) {
            $sa_secret = (subst $cap.resources.sa_token_secret $vars $name)
        }
        for p in ($cap.resources.pvcs? | default []) {
            $pvcs = ($pvcs | append { name: (subst $p.name $vars $name), storage_class: $cluster.storage.class, size: $cluster.storage.size })
        }
        for r in ($cap.resources.cluster_roles? | default []) {
            $cluster_roles = ($cluster_roles | append { name: (subst $r.name $vars $name), rules: $r.rules, from: $n })
        }
    }

    # What the cluster supplies (its administrators, once the admin flow has made them) replaces a same-named entry of the
    # target's config; infra/cluster-inputs.nu.
    let supplied = (env_for (read_inputs (path_of $INFRA $target)) $name)
    for e in (($entry.env? | default []) ++ (env_merged ($ov.env? | default []) $supplied)) {
        $env_list = ($env_list | append (env_entry $e $vars "entry" $name))
    }

    # A workload that answers requests on a port: the container declares it, a Service exposes it,
    # and the variable the agent reads it from is given the same number, so they cannot differ.
    let service = ($entry.service? | default null)
    if $service != null {
        # A port is a number, or text naming a service's port (`{cassini.port}`), so the number is
        # written once, in services.nuon.
        let service_ports = ($service.ports | each {|p|
            let port = if ($p.port | describe) == "string" { subst $p.port $vars $name | into int } else { $p.port }
            { name: ($p.name? | default null), port: $port }
        })
        $ports = ($service_ports | get port)
        $exposed = [{ name: (if ($service.name? | is-empty) { $name } else { subst $service.name $vars $name }), selector: $name, ports: $service_ports }]
        if ($service.port_variable? | is-not-empty) {
            $env_list = ($env_list | append { name: $service.port_variable, value: (($service_ports | first).port | into string), secret: null, key: null, from: "service" })
        }
    }

    # Shutdown: the grace period is derived, never set on its own, so it cannot
    # disagree with the budget.
    # The budget defaults to the least the code says it needs; the entry sets it only to give more.
    let explicit_budget = (($ov.shutdown?.budget_secs? | default ($entry.shutdown?.budget_secs? | default null)))
    let sd = if $explicit_budget == null {
        $spec.shutdown | upsert budget_secs (budget_minimum_of $scaffold)
    } else {
        $spec.shutdown
    }
    let grace = ($sd.budget_secs + $sd.grace_margin_secs)

    # Health: the files and thresholds HealthCheckActor and polar-healthcheck read.
    let h = $spec.health
    $volumes = ($volumes | append { name: $h.volume, secret: null, audience: null, from: "kind" })
    $mounts = ($mounts | append { name: $h.volume, path: $h.dir, read_only: false, from: "kind" })
    let cert_files = ($cert_dirs | str join ",")
    let dep_list = ($deps | get endpoint | str join ",")
    for e in [
        { name: "POLAR_HEALTH_FILE", value: $"($h.dir)/polar-health.json" }
        { name: "POLAR_HEALTH_CERTS", value: $cert_files }
        { name: "POLAR_HEALTH_EXPIRY_SECS", value: ($h.expiry_secs | into string) }
        { name: "POLAR_HEALTH_TICK_SECS", value: ($h.tick_secs | into string) }
        { name: "POLAR_HEALTH_REJUVENATION_SECS", value: ($h.rejuvenation_secs | into string) }
        { name: "POLAR_HEALTH_STALE_SECS", value: ($h.stale_secs | into string) }
        # The most the agent may take to drain. Its code declares the least it
        # needs; the agent refuses to start if this is less.
        { name: "POLAR_SHUTDOWN_BUDGET_SECS", value: ($sd.budget_secs | into string) }
        { name: "POLAR_HEALTH_DEP_ENDPOINTS", value: $dep_list }
        # What this deployment provides. The agent builder compares it with
        # what the code requires and refuses to start on a mismatch.
        { name: "POLAR_CAPABILITIES", value: ($needs | str join ",") }
    ] {
        $env_list = ($env_list | append (env_entry $e $vars "kind" $name))
    }

    # The agent reads one document, not an environment: the entries above are written into the
    # variables they name, and the pod is given the document and the secrets it refers to.
    let entries = $env_list
    let filled = (fill_document $scaffold $env_list $name)
    mut config_problems: list<string> = $filled.problems
    if $sd.budget_secs < (budget_minimum_of $scaffold) {
        $config_problems = ($config_problems | append $"($name) needs at least (budget_minimum_of $scaffold)s to drain, and the catalog gives (($sd.budget_secs))s")
    }
    let config_map = { name: $"($name)-config", key: $DOCUMENT_FILE, content: (document_json $filled) }
    $env_list = (pod_env $filled $"($DOCUMENT_DIR)/($DOCUMENT_FILE)")
    $volumes = ($volumes | append { name: $DOCUMENT_VOLUME, secret: null, audience: null, config_map: $config_map.name, from: "document" })
    $mounts = ($mounts | append { name: $DOCUMENT_VOLUME, path: $DOCUMENT_DIR, read_only: true, from: "document" })
    $volumes = ($volumes | each {|v| $v | default null config_map | default null claim | default [] items })

    let ci = ($vars | get cert-issuer)
    let scheme = ($services | get cert-issuer | get -o scheme | default "https")

    # Where polar-init gets this pod's certificates. On the credentials agent it is told the observer's
    # address and is not told cert-issuer's at all, so nothing in the pod can reach for it.
    let issuer = $spec.issuer
    let init_env = (if $issuer == "credentials-agent" {
        let obs = ($vars | get -o credentials-agent-observer)
        if $obs == null { error make { msg: $"($name): issuer credentials-agent needs credentials-agent-observer in services.nuon" } }
        $init_env
        | append { name: "POLAR_INIT_ISSUER", value: "credentials-agent", secret: null, key: null, from: "issuer" }
        | append { name: "POLAR_INIT_OBSERVER_URL", value: $obs.url, secret: null, key: null, from: "issuer" }
    } else { $init_env })

    {
        name: $name
        kind: $entry.kind
        namespace: $spec.namespace
        image: (image_ref $images $cluster $spec.image $name)
        pull_policy: $spec.pull_policy
        pull_secrets: $spec.pull_secrets
        polar_init_image: (image_ref $images $cluster "polar-init" $name)
        issuer: $issuer
        bootstrap: $spec.bootstrap
        cert_issuer_url: (if $issuer == "credentials-agent" { "" } else { $"($scheme)://($ci.addr)" })
        sa_token_audience: $cluster.sa_token_audience
        sa_token_volume: $spec.sa_token.volume
        sa_token_path: $spec.sa_token.path
        sa_token_expiration_secs: $spec.sa_token.expiration_secs
        service_account: ($spec.service_account? | default $name)
        sa_token_secret: $sa_secret
        automount_sa_token: (if $sa_secret == null { false } else { null })
        restart_policy: $spec.restart_policy
        backoff_limit: $spec.backoff_limit
        ttl_after_finished_secs: $spec.ttl_after_finished_secs
        probe: $spec.probe
        termination_grace_secs: $grace
        proxy_ca_cert: null
        needs: $needs
        env: $env_list
        volumes: $volumes
        mounts: $mounts
        init_mounts: $init_mounts
        init_env: $init_env
        ports: $ports
        dep_checks: ($deps | where {|d| not ($d.service in $no_start_wait) } | get endpoint)
        certs: ($certs | get spec)
        resources: { cluster_roles: ($cluster_roles | reject from), sa_token_secrets: (if $sa_secret == null { [] } else { [$sa_secret] }), pvcs: $pvcs, services: $exposed }
        config_map: $config_map
        config_problems: $config_problems
        check_names: (check_names $spec)
        _origin: $origin
        _settings: { issuer: $issuer, bootstrap: $spec.bootstrap, health: $h, shutdown: ($sd | insert termination_grace_secs $grace) }
        _from: { roles: $cluster_roles, env: $entries, volumes: $volumes, mounts: $mounts, deps: $deps, certs: $certs }
    }
}

# What the template takes: the resolved record without provenance.
def for_template [r: record] {
    $r
    | reject _origin _from _settings resources config_map config_problems check_names issuer bootstrap
    | update env { reject from }
    | update volumes { reject from }
    | update mounts { reject from }
    | update init_mounts { reject from }
    | update init_env { reject from }
}

# ── Validation ───────────────────────────────────────────────────────────────

# The capability grid: every capability answers every facet in facets.nuon,
# and answers no facet that is not listed there. Also that each dependency
# names a real service and each placeholder in a capability can be resolved.
def check_capabilities [] {
    let facets = (open ($CONFIG | path join "facets.nuon") | columns)
    let caps = (open ($CONFIG | path join "capabilities.nuon"))
    let services = (open ($CONFIG | path join "services.nuon"))
    let vars = (service_vars $services (load_cluster $DEFAULT_TARGET))

    $caps | transpose name cap | each {|c|
        let have = ($c.cap | columns)
        let missing = ($facets | where {|f| not ($f in $have) })
        let unknown = ($have | where {|f| not ($f in $facets) })
        let wrong = ($facets | where {|f| ($f in $have) and not (($c.cap | get $f | describe) | str starts-with "record") })
        let svc = ($c.cap.dependency?.service? | default null)

        let found = (
            (if ($missing | is-not-empty) { [$"capability '($c.name)' does not answer facet\(s\): ($missing | str join ', ')"] } else { [] })
            ++ (if ($unknown | is-not-empty) { [$"capability '($c.name)' answers unknown facet\(s\): ($unknown | str join ', ') \(facets.nuon lists: ($facets | str join ', ')\)"] } else { [] })
            ++ (if ($wrong | is-not-empty) { [$"capability '($c.name)': facet\(s\) ($wrong | str join ', ') must be a record \(use {} for nothing\)"] } else { [] })
            ++ (if ($svc != null and ($vars | get -o $svc) == null) { [$"capability '($c.name)': dependency names service '($svc)', which is not in services.nuon"] } else { [] })
        )
        # Resolving every string catches a placeholder naming a missing service.
        let texts = (
            ($c.cap.pod?.env? | default [] | each {|e| $e.value? | default "" })
            ++ ($c.cap.pod?.volumes? | default [] | each {|v| $v.name })
        )
        let unresolved = ($texts | each {|t|
            try { subst $t $vars $"capability ($c.name)"; null } catch {|e| $e.msg }
        } | compact)
        $found ++ $unresolved
    } | flatten
}

# Everything that can be wrong with the configuration, found without a cluster.
def check_all [target: string] {
    let names = (all_workloads $target | get name)
    mut problems: list<any> = []

    for p in (enabled_problems $target) { $problems = ($problems | append $p) }

    for p in (check_capabilities) { $problems = ($problems | append $p) }

    let dupes = (catalog_workloads | get name | uniq --repeated)
    for d in $dupes { $problems = ($problems | append $"workload name '($d)' is defined more than once") }

    let results = ($names | each {|n|
        try {
            let r = (resolve $n $target)
            {
                who: $n
                issuer: $r.issuer
                bootstrap: $r.bootstrap
                identity: $"($r.service_account).($r.namespace)"
                # The agent's configuration is judged by filling its own scaffold and then by the agent
                # itself; its pod environment is a pointer, not the variables.
                rules: ($r.config_problems ++ (agent_check $n $r) ++ (shutdown_problems $n $r._settings.shutdown))
                error: null
            }
        } catch {|e|
            { who: $n, issuer: null, bootstrap: null, identity: null, rules: [], error: $e.msg }
        }
    })
    for r in ($results | where error != null) { $problems = ($problems | append $r.error) }
    for r in $results { for p in $r.rules { $problems = ($problems | append $p) } }
    let sas = ($results | where error == null)

    # A workload's ServiceAccount becomes the SAN of its certificate, so two
    # workloads sharing one are indistinguishable. render.nu checks this on the
    # manifests; here it fails before anything is rendered.
    let shared = ($sas | group-by identity | transpose identity items | where {|g| ($g.items | length) > 1 })
    for g in $shared {
        $problems = ($problems | append $"identity ($g.identity) is shared by: ($g.items | get who | str join ', ')")
    }
    # Where each workload gets its certificates, and the rule that only the bootstrap identities may ask
    # cert-issuer directly (an error once the floor is enforced, a list of what is still to move until then).
    let issuance = (issuer_problems ($sas | select who issuer bootstrap) ((load_cluster $target).issuance?.floor_enforced? | default false))
    for p in $issuance.errors { $problems = ($problems | append $p) }
    { problems: $problems, pending: $issuance.pending }
}

# What is wrong with how a target says which workloads it runs: `enabled` is true or false and nothing else, and
# a workload it disables is not still in its apply order (its manifests are not rendered, so `up.nu` would skip
# them without a word).
def enabled_problems [target: string] {
    let overrides = (target_config $target | get -o workloads | default {} | transpose name override
        | where {|w| $w.override | describe | str starts-with "record" })
    mut problems: list<string> = []
    for w in $overrides {
        let enabled = ($w.override | get -o enabled)
        if $enabled != null and ($enabled | describe) != "bool" {
            $problems = ($problems | append $"target ($target): workloads.($w.name).enabled must be true or false")
        }
    }
    let target_file = ($INFRA | path join $"targets/($target)/target.nu")
    if ($target_file | path exists) {
        let order = (try { ^$nu.current-exe $target_file | from nuon | get -o apply_order | default [] } catch { [] })
        for n in (disabled_in $target) {
            for file in [$"($n)-resources.yaml" $"($n).yaml"] {
                if $file in $order {
                    $problems = ($problems | append $"target ($target): apply_order lists ($file), but the target disables ($n)")
                }
            }
        }
    }
    $problems
}

# The floor of cert-issuer for this target, written from the catalog: what each workload is issued, resolved.
def floor_of [target: string] {
    let records = (all_workloads $target | get name | each {|n|
        let r = (resolve $n $target)
        { who: $n, namespace: $r.namespace, service_account: $r.service_account, issuer: $r.issuer, bootstrap: $r.bootstrap, certs: $r.certs }
    })
    floor_entries $records
}

# ── Comparison with a rendered manifest ──────────────────────────────────────

# The same facts as resolve produces, read back from a rendered Job manifest.
def actual_of [file: string] {
    let doc = (open --raw $file | from yaml)
    let pod = $doc.spec.template.spec
    let c = ($pod.containers | first)
    let init = ($pod.initContainers | first)

    let pairs = ($init.args | window 2 --stride 2)
    {
        image: $c.image
        pull_policy: $c.imagePullPolicy
        polar_init_image: $init.image
        service_account: $pod.serviceAccountName
        automount_sa_token: ($pod | get -o automountServiceAccountToken)
        restart_policy: $pod.restartPolicy
        backoff_limit: $doc.spec.backoffLimit
        ttl_after_finished_secs: $doc.spec.ttlSecondsAfterFinished
        termination_grace_secs: ($pod | get -o terminationGracePeriodSeconds)
        env: ($c.env | each {|e|
            {
                name: $e.name
                value: ($e | get -o value)
                secret: ($e | get -o valueFrom | get -o secretKeyRef | get -o name)
                key: ($e | get -o valueFrom | get -o secretKeyRef | get -o key)
            }
        })
        volumes: ($pod.volumes | each {|v|
            {
                name: $v.name
                secret: ($v | get -o secret | get -o secretName)
                audience: ($v | get -o projected | get -o sources | get -o 0 | get -o serviceAccountToken | get -o audience)
            }
        })
        mounts: ($c.volumeMounts | each {|m| { name: $m.name, path: $m.mountPath, read_only: (($m | get -o readOnly) == true) } })
        init_mounts: ($init.volumeMounts | where name != "sa-token" | each {|m| { name: $m.name, path: $m.mountPath } })
        dep_checks: ($pairs | where {|p| $p.0 == "--check-dep-cert" } | each {|p| $p.1 })
        certs: ($pairs | where {|p| $p.0 == "--cert" } | each {|p| $p.1 })
    }
}

def expected_of [r: record] {
    {
        image: $r.image
        pull_policy: $r.pull_policy
        polar_init_image: $r.polar_init_image
        service_account: $r.service_account
        automount_sa_token: $r.automount_sa_token
        restart_policy: $r.restart_policy
        backoff_limit: $r.backoff_limit
        ttl_after_finished_secs: $r.ttl_after_finished_secs
        termination_grace_secs: $r.termination_grace_secs
        env: ($r.env | each {|e| { name: $e.name, value: $e.value, secret: $e.secret, key: $e.key } })
        volumes: ($r.volumes | each {|v| { name: $v.name, secret: $v.secret, audience: $v.audience } })
        mounts: ($r.mounts | each {|m| { name: $m.name, path: $m.path, read_only: $m.read_only } })
        init_mounts: ($r.init_mounts | each {|m| { name: $m.name, path: $m.path } })
        dep_checks: $r.dep_checks
        certs: $r.certs
    }
}

# Order inside these lists carries no meaning, so compare them as multisets;
# anything else is compared as it is.
const LIST_FIELDS = [env volumes mounts init_mounts dep_checks certs]

def one_line [x: any] { $x | to json --raw }

# Prints the differences and returns how many there are.
def compare [expected: record, actual: record] {
    mut n = 0
    for k in ($expected | columns) {
        let e = ($expected | get $k)
        let a = ($actual | get $k)
        if $k in $LIST_FIELDS {
            let es = ($e | each {|x| one_line $x })
            let as = ($a | each {|x| one_line $x })
            let only_e = ($es | where {|x| not ($x in $as) })
            let only_a = ($as | where {|x| not ($x in $es) })
            for x in $only_e { print $"    + ($k): ($x)"; $n += 1 }
            for x in $only_a { print $"    - ($k): ($x)"; $n += 1 }
        } else if (one_line $e) != (one_line $a) {
            print $"    ~ ($k): rendered (one_line $a), resolved (one_line $e)"
            $n += 1
        }
    }
    $n
}

# ── Entry points ─────────────────────────────────────────────────────────────

def main [] { help main }

# List the workloads a target runs.
def "main list" [--target: string = $DEFAULT_TARGET] {
    all_workloads $target | select name kind image | each {|w| $w }
}

# The images the agents of the catalog run, as a JSON array. Those whose workloads say
# `config: "document"` are asked for their scaffold when the catalog is rendered, so they have to
# be built first: `up.nu` builds exactly these before it renders.
def "main document-images" [--target: string = $DEFAULT_TARGET] {
    print (all_workloads $target | where {|w| ($w.config? | default "env") == "document" } | get image | uniq | to json --raw)
}

# The workload names, as a JSON array (for scripts that render each one).
def "main names" [--target: string = $DEFAULT_TARGET] {
    print (all_workloads $target | get name | to json --raw)
}

# Writes the scaffold of every agent in the catalog to `dir`, one `<agent>.json` each: what `polar-sign template`
# and `polar-sign sign` read to know what each agent takes (its runtime sections, the settings a signed document
# may change, their defaults and which fields are secret). An author keeps them in `scaffolds/` next to their
# certificate (`~/.config/polar-admin/scaffolds`). The scaffold comes from the agent itself (the source with
# cargo, the built image with POLAR_AGENT_SOURCE=images), never from a copy.
def "main scaffolds" [dir: string, --target: string = $DEFAULT_TARGET] {
    mkdir $dir
    let agents = (all_workloads $target | where kind == "agent")
    for w in $agents {
        let scaffold = (scaffold_of $w.name $w)
        let name = ($scaffold | get agent.name)
        $scaffold | to json | save --force ($dir | path join $"($name).json")
        print $"  ($name)"
    }
    print $"wrote (($agents | length)) scaffold\(s\) to ($dir)"
}

# Show what a workload resolves to and where each value came from.
def "main explain" [
    name: string
    --target: string = $DEFAULT_TARGET
] {
    let r = (resolve $name $target)
    print $"($r.name)  \(kind ($r.kind), target ($target)\)"
    print $"  needs: ($r.needs | str join ', ')"
    print ""
    print "  values:"
    for k in [namespace image pull_policy polar_init_image issuer bootstrap cert_issuer_url service_account sa_token_secret automount_sa_token restart_policy backoff_limit ttl_after_finished_secs termination_grace_secs] {
        let v = ($r | get $k)
        let o = ($r._origin | get -o $k | default "derived")
        print $"    ($k | fill -w 24)  (one_line $v)   [($o)]"
    }
    print ""
    print "  env:"
    for e in $r._from.env {
        let v = if $e.secret != null { $"secret ($e.secret)/($e.key)" } else { $e.value }
        print $"    ($e.name | fill -w 28)  ($v)   [($e.from)]"
    }
    print ""
    print $"  document: ($r.config_map.name)/($r.config_map.key), read by the agent through POLAR_CONFIG"
    print "    the env above is written into the variables the agent's own scaffold declares;"
    print "    the pod is given the pointer and the secrets it refers to:"
    for e in $r.env { print $"      ($e.name | fill -w 24)  (if $e.secret != null { $"secret ($e.secret)/($e.key)" } else { $e.value })" }
    if ($r.config_problems | is-not-empty) {
        print "    problems:"
        for p in $r.config_problems { print $"      ($p)" }
    }
    print ""
    print "  volumes:"
    for v in $r._from.volumes {
        let what = if $v.secret != null { $"secret ($v.secret)" } else if ($v.claim? | default null) != null { $"persistent volume claim ($v.claim)" } else if ($v.config_map? | default null) != null { $"ConfigMap ($v.config_map)" } else if $v.audience != null { $"projected token, audience ($v.audience)" } else { "emptyDir" }
        print $"    ($v.name | fill -w 24)  ($what)   [($v.from)]"
    }
    print ""
    print "  container mounts:"
    for m in $r._from.mounts {
        let ro = if $m.read_only { " (read-only)" } else { "" }
        print $"    ($m.name | fill -w 24)  ($m.path)($ro)   [($m.from)]"
    }
    print ""
    print "  polar-init:"
    for d in $r._from.deps { print $"    --check-dep-cert ($d.endpoint)   [($d.from)]" }
    for c in $r._from.certs { print $"    --cert ($c.spec)   [($c.from)]" }
    print ""
    print "  resources:"
    for s in $r.resources.sa_token_secrets { print $"    Secret ($s) \(service-account-token\)" }
    for p in $r.resources.pvcs { print $"    PersistentVolumeClaim ($p.name) \(($p.size), class ($p.storage_class)\)" }
    for s in $r.resources.services { print $"    Service ($s.name) on port ($s.port)" }
    for c in $r._from.roles {
        let rules = ($c.rules | each {|x| $"($x.verbs | str join '/') on ($x.resources | str join ',')" } | str join "; ")
        print $"    ClusterRole ($c.name) bound to ($r.service_account): ($rules)   [($c.from)]"
    }
    if ($r.resources.sa_token_secrets | is-empty) and ($r._from.roles | is-empty) and ($r.resources.pvcs | is-empty) and ($r.resources.services | is-empty) { print "    (none)" }
}

# The health and shutdown settings the rules in rules.nu read, as JSON.
def "main settings" [
    name: string
    --target: string = $DEFAULT_TARGET
] {
    (resolve $name $target)._settings | to json
}

# The names the agent check uses for a workload, as JSON: the image it pulls and the crate it builds.
def "main agent-names" [
    name: string
    --target: string = $DEFAULT_TARGET
] {
    (resolve $name $target).check_names | to json
}

# The record the workload's template takes, as JSON.
def "main json" [
    name: string
    --target: string = $DEFAULT_TARGET
] {
    for_template (resolve $name $target) | to json
}

# The Kubernetes objects that exist alongside a workload (RBAC, token Secrets),
# as the record schema/resources.dhall takes. Empty lists mean nothing to render.
def "main resources" [
    name: string
    --target: string = $DEFAULT_TARGET
] {
    let r = (resolve $name $target)
    {
        name: $r.name
        namespace: $r.namespace
        service_account: $r.service_account
        cluster_roles: $r.resources.cluster_roles
        sa_token_secrets: $r.resources.sa_token_secrets
        pvcs: $r.resources.pvcs
        services: $r.resources.services
        config_maps: [$r.config_map]
    } | to json
}

# Validate every workload in the catalog.
def "main check" [--target: string = $DEFAULT_TARGET] {
    let found = (check_all $target)
    let problems = $found.problems
    if ($found.pending | is-not-empty) {
        print $"pending: ($found.pending | length) workload\(s\) still ask cert-issuer directly and are not bootstrap identities; the floor cannot be enforced until they move"
        for p in $found.pending { print $"  ($p)" }
    }
    if ($problems | is-empty) {
        print $"ok: (all_workloads $target | length) workloads resolve cleanly for ($target)"
    } else {
        for p in $problems { print -e $"  ERROR: ($p)" }
        exit 1
    }
}

# The entries of cert-issuer's floor for this target, as JSON (render.nu hands them to the cert-issuer chart).
def "main floor" [--target: string = $DEFAULT_TARGET] {
    let f = (floor_of $target)
    if ($f.problems | is-not-empty) {
        for p in $f.problems { print -e $"  ERROR: ($p)" }
        exit 1
    }
    print ($f.entries | to json --raw)
}

# The URL of an in-cluster service, from services.nuon (render.nu hands it to the hand-written charts, so the
# address is written once). An error for a service that is not there or is not addressed by URL.
def "main service-url" [name: string, --target: string = $DEFAULT_TARGET] {
    let services = (open ($CONFIG | path join "services.nuon"))
    let vars = (service_vars $services (load_cluster $target))
    let url = ($vars | get -o $name | get -o url)
    if $url == null {
        error make { msg: $"service '($name)' is not in services.nuon, or has no scheme and so no URL" }
    }
    print $url
}

# Whether cert-issuer enforces the floor on this target (cluster.nuon, `issuance.floor_enforced`).
def "main floor-enforced" [--target: string = $DEFAULT_TARGET] {
    print ((load_cluster $target).issuance?.floor_enforced? | default false)
}

# Compare a workload's resolved record with the manifest it is rendered into.
# + is in the resolved record but not the manifest (a render would add it);
# - is in the manifest but not the resolved record (a render would drop it).
def "main diff" [
    name: string
    --target: string = $DEFAULT_TARGET
    --manifests: string = "manifests"
] {
    let file = ($manifests | path join $"($name).yaml")
    if not ($file | path exists) {
        error make { msg: $"($file) not found. Render first: nu infra/render.nu ($target)" }
    }
    print $"($name)  vs  ($file)"
    let n = (compare (expected_of (resolve $name $target)) (actual_of $file))
    if $n == 0 { print "    identical" }
}
