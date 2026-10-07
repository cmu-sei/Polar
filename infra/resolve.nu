#!/usr/bin/env nu
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

const INFRA = (path self | path dirname)
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

def all_workloads [] {
    glob ($CONFIG | path join "workloads/*.nuon") | sort | each {|f| open $f } | flatten
}

# <service> -> { dns, port, addr }, from services.nuon and the cluster domain.
def service_vars [services: record, cluster: record] {
    $services
    | transpose name s
    | reduce --fold {} {|it, acc|
        let ns = ($cluster.namespaces | get $it.s.namespace)
        let dns = $"($it.s.dns_name).($ns).svc.($cluster.domain)"
        $acc | insert $it.name { dns: $dns, port: $it.s.port, addr: $"($dns):($it.s.port)" }
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
            | str replace --all $"{($it.name).dns}" $it.v.dns
            | str replace --all $"{($it.name).port}" ($it.v.port | into string)
            | str replace --all $"{($it.name).addr}" $it.v.addr
        }
    )
    if ($out =~ '\{[A-Za-z0-9-]+\.(dns|port|addr)\}') {
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

# ── Resolution ───────────────────────────────────────────────────────────────

export def resolve [name: string, target: string = $DEFAULT_TARGET] {
    let cluster = (load_cluster $target)
    let services = (open ($CONFIG | path join "services.nuon"))
    let caps = (open ($CONFIG | path join "capabilities.nuon"))
    let kinds = (open ($CONFIG | path join "kinds.nuon"))
    let images = (open ($INFRA | path join "images.nuon"))
    let vars = (service_vars $services $cluster)

    let found = (all_workloads | where name == $name)
    if ($found | is-empty) {
        error make { msg: $"no workload named '($name)'. Known: (all_workloads | get name | str join ', ')" }
    }
    let entry = ($found | first)
    let ov = (target_config $target | get -o workloads | default {} | get -o $name | default {})

    if not ($entry.kind in ($kinds | columns)) {
        error make { msg: $"($name): unknown kind '($entry.kind)'" }
    }
    let kind = ($kinds | get $entry.kind)

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

    let needs = ($kind.needs ++ ($entry.needs? | default []) ++ ($ov.needs? | default []) | uniq)

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
            $deps = ($deps | append { endpoint: $"($svc.addr):($dep.ttl_secs)", from: $n })
        }

        # identity: client certificates, each with its volume and mounts.
        for c in ($cap.identity.certs? | default []) {
            $volumes = ($volumes | append { name: $c.volume, secret: null, audience: null, from: $n })
            $mounts = ($mounts | append { name: $c.volume, path: $c.dir, read_only: $c.read_only, from: $n })
            $init_mounts = ($init_mounts | append { name: $c.volume, path: $c.dir, from: $n })
            $certs = ($certs | append { spec: $"client:($c.dir):ecdsa-p256:", from: $n })
            $cert_dirs = ($cert_dirs | append $c.dir)
        }

        # pod: what the agent container gets.
        for e in ($cap.pod.env? | default []) {
            $env_list = ($env_list | append (env_entry $e $vars $n $name))
        }
        for v in ($cap.pod.volumes? | default []) {
            let sec = (if ($v.secret? | is-empty) { null } else { subst $v.secret $vars $name })
            $volumes = ($volumes | append { name: (subst $v.name $vars $name), secret: $sec, audience: null, from: $n })
        }
        for m in ($cap.pod.mounts? | default []) {
            $mounts = ($mounts | append { name: (subst $m.name $vars $name), path: $m.path, read_only: $m.read_only, from: $n })
        }

        # resources: objects that live alongside the workload.
        if ($cap.resources.sa_token_secret? | is-not-empty) {
            $sa_secret = (subst $cap.resources.sa_token_secret $vars $name)
        }
        for r in ($cap.resources.cluster_roles? | default []) {
            $cluster_roles = ($cluster_roles | append { name: (subst $r.name $vars $name), rules: $r.rules, from: $n })
        }
    }

    for e in (($entry.env? | default []) ++ ($ov.env? | default [])) {
        $env_list = ($env_list | append (env_entry $e $vars "entry" $name))
    }

    # Shutdown: the grace period is derived, never set on its own, so it cannot
    # disagree with the budget.
    let sd = $spec.shutdown
    let grace = ($sd.budget_secs + $sd.grace_margin_secs)

    # Health: the files and thresholds HealthCheckActor and polar-healthcheck read.
    let h = $spec.health
    $volumes = ($volumes | append { name: $h.volume, secret: null, audience: null, from: "kind" })
    $mounts = ($mounts | append { name: $h.volume, path: $h.dir, read_only: false, from: "kind" })
    let cert_files = ($cert_dirs | each {|d| $"($d)/cert.pem" } | str join ",")
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

    let ci = ($vars | get cert-issuer)
    let scheme = ($services | get cert-issuer | get -o scheme | default "https")

    {
        name: $name
        kind: $entry.kind
        namespace: $spec.namespace
        image: (image_ref $images $cluster $spec.image $name)
        pull_policy: $spec.pull_policy
        pull_secrets: $spec.pull_secrets
        polar_init_image: (image_ref $images $cluster "polar-init" $name)
        cert_issuer_url: $"($scheme)://($ci.addr)"
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
        dep_checks: ($deps | get endpoint)
        certs: ($certs | get spec)
        resources: { cluster_roles: ($cluster_roles | reject from), sa_token_secrets: (if $sa_secret == null { [] } else { [$sa_secret] }) }
        _origin: $origin
        _settings: { health: $h, shutdown: ($sd | insert termination_grace_secs $grace) }
        _from: { roles: $cluster_roles, env: $env_list, volumes: $volumes, mounts: $mounts, deps: $deps, certs: $certs }
    }
}

# What the template takes: the resolved record without provenance.
def for_template [r: record] {
    $r
    | reject _origin _from _settings resources
    | update env { reject from }
    | update volumes { reject from }
    | update mounts { reject from }
    | update init_mounts { reject from }
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
    let names = (all_workloads | get name)
    mut problems: list<any> = []

    for p in (check_capabilities) { $problems = ($problems | append $p) }

    let dupes = ($names | uniq --repeated)
    for d in $dupes { $problems = ($problems | append $"workload name '($d)' is defined more than once") }

    let results = ($names | each {|n|
        try {
            let r = (resolve $n $target)
            {
                who: $n
                identity: $"($r.service_account).($r.namespace)"
                rules: ((environment_problems $n $r.env) ++ (shutdown_problems $n $r._settings.shutdown))
                error: null
            }
        } catch {|e|
            { who: $n, identity: null, rules: [], error: $e.msg }
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
    $problems
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

# List the workloads in the catalog.
def "main list" [] {
    all_workloads | select name kind image | each {|w| $w }
}

# The workload names, as a JSON array (for scripts that render each one).
def "main names" [--target: string = $DEFAULT_TARGET] {
    print (all_workloads | get name | to json --raw)
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
    for k in [namespace image pull_policy polar_init_image cert_issuer_url service_account sa_token_secret automount_sa_token restart_policy backoff_limit ttl_after_finished_secs termination_grace_secs] {
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
    print "  volumes:"
    for v in $r._from.volumes {
        let what = if $v.secret != null { $"secret ($v.secret)" } else if $v.audience != null { $"projected token, audience ($v.audience)" } else { "emptyDir" }
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
    for c in $r._from.roles {
        let rules = ($c.rules | each {|x| $"($x.verbs | str join '/') on ($x.resources | str join ',')" } | str join "; ")
        print $"    ClusterRole ($c.name) bound to ($r.service_account): ($rules)   [($c.from)]"
    }
    if ($r.resources.sa_token_secrets | is-empty) and ($r._from.roles | is-empty) { print "    (none)" }
}

# The health and shutdown settings the rules in rules.nu read, as JSON.
def "main settings" [
    name: string
    --target: string = $DEFAULT_TARGET
] {
    (resolve $name $target)._settings | to json
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
    } | to json
}

# Validate every workload in the catalog.
def "main check" [--target: string = $DEFAULT_TARGET] {
    let problems = (check_all $target)
    if ($problems | is-empty) {
        print $"ok: (all_workloads | length) workloads resolve cleanly for ($target)"
    } else {
        for p in $problems { print -e $"  ERROR: ($p)" }
        exit 1
    }
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
