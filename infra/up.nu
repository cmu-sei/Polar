#!/usr/bin/env nu
# infra/up.nu
#
# Brings a Polar cluster up from a target, end to end. Run it from the repo
# root with KUBECONFIG pointing at the cluster:
#
#   nu infra/up.nu local-minimal --dev --user djshepard --email djshepard@sei.cmu.edu
#
# It is target-aware: what to do is derived from the target's own config (the
# charts it deploys, its apply order, whether it provisions the cluster CA), so
# a step that the target does not need is skipped, with the reason printed in
# the plan up front. infra/render.nu only renders; this script applies.
#
# Every step checks its own precondition, so after a failure or a pause you can
# re-run the same command and it resumes. It stops for a human only where a
# human is required (closing the browser, the Neo4j password).
#
# Steps (each enabled only when the target needs it):
#   preflight   cluster reachable, run from the repo root                 always
#   render      nu infra/render.nu <target>                               always
#   images      build and push only what the rendered manifests reference --dev
#   namespaces  kubectl apply namespaces.yaml                             namespaces.yaml in apply_order
#   ca          polar-admin-grant bootstrap-ca                            cert-issuer deployed and cert_issuer_ca_configmap set
#   infra       target secrets.nu, then every manifest in apply order     always
#   pods        wait for the deployed core services                       any of cert-issuer, cassini, credentials-agent, neo4j
#   forwards    just port-forwards-up <only what is needed>               credentials-agent (observer) or neo4j
#   admin       admin-bootstrap / admin-provision / admin-claim           credentials-agent deployed
#   browser     user-cert-install, then the Neo4j password                credentials-agent and neo4j deployed

const POLAR = "polar"
const GRAPH = "polar-graph"

use images.nu

def note [msg: string] { print $"  -- ($msg)" }
def ok [msg: string] { print $"  ok: ($msg)" }

def fail [msg: string] {
    print -e $"  FAILED: ($msg)"
    exit 1
}

# Stops for a human. Re-running the script resumes at the next unfinished step.
def pause [msg: string] {
    print ""
    print $"  PAUSE: ($msg)"
    try {
        input "         Press Enter when done (Ctrl-C to stop; re-run to resume): " | ignore
    } catch {
        print ""
        print "  Stopped. Re-run the same command to resume where you left off."
        exit 130
    }
}

def admin_dir [] {
    $env.HOME | path join ".config/polar-admin"
}

# Pipe a closure in; true when it exits 0.
def cmd_ok [] {
    let closure = $in
    let r = (do $closure | complete)
    $r.exit_code == 0
}

# kubectl apply, deleting first when the manifest holds a Job (their pod
# templates are immutable).
def apply_file [output_dir: string, filename: string] {
    let manifest = ($output_dir | path join $filename)
    if not ($manifest | path exists) {
        note $"skipping ($filename), not found"
        return
    }
    print $"  applying ($filename)..."
    let has_job = (open --raw $manifest
        | find --regex r#'(?m)^\s*["']?kind["']?\s*:\s*["']?Job["']?\s*,?\s*$'#
        | is-not-empty)
    if $has_job {
        ^kubectl delete --ignore-not-found -f $manifest
    }
    ^kubectl apply -f $manifest
    if $env.LAST_EXIT_CODE != 0 { fail $"kubectl apply failed for ($filename)" }
}

# True when at least one pod matching the selector is Running with every
# container ready.
def pod_ready [ns: string, selector: string] {
    let r = (do { ^kubectl -n $ns get pods -l $selector -o json } | complete)
    if $r.exit_code != 0 { return false }
    $r.stdout | from json | get items | any {|p|
        let statuses = ($p.status.containerStatuses? | default [])
        ($p.status.phase == "Running") and ($statuses | is-not-empty) and ($statuses | all {|c| $c.ready })
    }
}

def wait_pods [waits: list, timeout: duration] {
    let deadline = ((date now) + $timeout)
    mut pending = ($waits | where kind == "pods")
    loop {
        $pending = ($pending | where {|w| not (pod_ready $w.ns $w.ref) })
        if ($pending | is-empty) { break }
        if (date now) > $deadline {
            fail $"timed out waiting for pods: ($pending | get label | str join ', ')"
        }
        print $"  waiting for: ($pending | get label | str join ', ')"
        sleep 5sec
    }
    for w in ($waits | where kind == "statefulset") {
        print $"  waiting for statefulset ($w.label)..."
        ^kubectl -n $w.ns rollout status $w.ref --timeout=600s
        if $env.LAST_EXIT_CODE != 0 { fail $"statefulset ($w.label) did not become ready" }
    }
    ok "required services are ready"
}

# What the target deploys, as chart directory names.
def deployed_charts [config: record] {
    $config.charts | each {|c| $c | path dirname | path basename }
}

# The plan: one record per step, with whether it runs and why not.
def build_plan [config: record, dev: bool] {
    let d = (deployed_charts $config)
    let order = $config.apply_order
    let ca_cm = ($config.cert_issuer_ca_configmap? | default "")
    let has_ci = ("cert-issuer" in $d)
    let has_ca_agent = ("credentials-agent" in $d)
    let has_neo4j = ("neo4j" in $d)
    let has_core = ($has_ci or ("cassini" in $d) or $has_ca_agent or $has_neo4j)

    [
        { id: "preflight", title: "preflight", on: true, why: "" }
        { id: "render", title: "render", on: true, why: "" }
        { id: "images", title: "images", on: $dev, why: "pass --dev to build and load images into the local registry" }
        { id: "namespaces", title: "namespaces", on: ("namespaces.yaml" in $order), why: "target has no namespaces.yaml in its apply order" }
        {
            id: "ca"
            title: "cluster CA"
            on: ($has_ci and ($ca_cm | is-not-empty))
            why: (if not $has_ci { "target does not deploy cert-issuer" } else { "target does not use a provisioned CA (cert_issuer_ca_configmap is empty)" })
        }
        { id: "infra", title: "secrets and infrastructure", on: true, why: "" }
        { id: "pods", title: "waiting for core services", on: $has_core, why: "target deploys none of cert-issuer, cassini, credentials-agent, neo4j" }
        { id: "forwards", title: "port-forwards", on: ($has_ca_agent or $has_neo4j), why: "target deploys neither credentials-agent nor neo4j" }
        { id: "admin", title: "admin flow", on: $has_ca_agent, why: "target does not deploy credentials-agent" }
        {
            id: "browser"
            title: "browser certificate and Neo4j password"
            on: ($has_ca_agent and $has_neo4j)
            why: (if not $has_neo4j { "target does not deploy neo4j" } else { "no credentials-agent, so no user certificate to import" })
        }
    ]
}

def print_plan [target: string, plan: list] {
    print $"=== Plan for target ($target) ==="
    for s in $plan {
        if $s.on {
            print $"  run   ($s.title)"
        } else {
            print $"  skip  ($s.title): ($s.why)"
        }
    }
}

# What each deployed service needs to be waited on.
def build_waits [config: record] {
    let d = (deployed_charts $config)
    mut w = []
    if "cert-issuer" in $d {
        $w = ($w | append { kind: "pods", ns: $POLAR, ref: "name=cert-issuer", label: "cert-issuer" })
    }
    if "cassini" in $d {
        $w = ($w | append { kind: "pods", ns: $POLAR, ref: "job-name=cassini", label: "cassini" })
    }
    if "credentials-agent" in $d {
        $w = ($w | append { kind: "pods", ns: $POLAR, ref: "job-name=credentials-agent", label: "credentials-agent" })
        $w = ($w | append { kind: "pods", ns: $POLAR, ref: "job-name=credentials-agent-observer", label: "credentials-agent-observer" })
    }
    if "neo4j" in $d {
        $w = ($w | append { kind: "statefulset", ns: $GRAPH, ref: "statefulset/polar-neo4j", label: "neo4j" })
    }
    $w
}

# Port-forwards needed by the admin and browser steps, and only those.
def build_forwards [config: record] {
    let d = (deployed_charts $config)
    mut f = []
    if "credentials-agent" in $d {
        $f = ($f | append { name: "observer", svc: "credentials-agent-observer" })
    }
    if "neo4j" in $d {
        $f = ($f | append { name: "neo4j", svc: "polar-db-svc" })
    }
    $f
}

def step_preflight [] {
    if not ("infra/render.nu" | path exists) {
        fail "run this from the repo root (infra/render.nu not found)"
    }
    if ($env.KUBECONFIG? | default "" | is-empty) {
        fail "KUBECONFIG is not set (fish: set -gx KUBECONFIG (pwd)/kubeconfig in the nix-usernetes directory)"
    }
    let nodes = (do { ^kubectl get nodes -o name } | complete)
    if $nodes.exit_code != 0 {
        fail $"cluster not reachable: ($nodes.stderr | str trim)"
    }
    ok $"cluster reachable, ($nodes.stdout | lines | length) node\(s\)"
}

def step_render [target: string] {
    ^$nu.current-exe infra/render.nu $target
    if $env.LAST_EXIT_CODE != 0 { fail "render failed" }
}

def step_images [config: record] {
    let files = ($config.apply_order
        | each {|f| $config.output_dir | path join $f }
        | where {|f| $f | path exists })
    ^just registry-up
    let rows = try { images needed $files } catch {|e| fail $e.msg }
    print $"  ($rows | length) image\(s\) needed by this target"
    try { images sync $rows } catch {|e| fail $e.msg }
    ok "images built and pushed"
}

def step_ca [] {
    let have_ca = ({ ^kubectl -n $POLAR get secret cert-issuer-ca -o name } | cmd_ok)
    if $have_ca {
        ok "secret/cert-issuer-ca already exists, skipping bootstrap-ca"
    } else {
        ^cargo run -q --manifest-path src/agents/Cargo.toml -p polar-admin-grant -- bootstrap-ca
        if $env.LAST_EXIT_CODE != 0 { fail "bootstrap-ca failed" }
        ok "cluster CA created"
    }
}

def step_infra [config: record] {
    let output_dir = $config.output_dir
    let secrets_script = ($config.target_dir | path join "secrets.nu")
    if ($secrets_script | path exists) {
        print "  creating secrets..."
        ^$nu.current-exe $secrets_script $config.target_dir $config.repo_root
        if $env.LAST_EXIT_CODE != 0 { fail "secrets.nu failed" }
    }
    for filename in ($config.apply_order | skip 1) {
        apply_file $output_dir $filename
    }
    # The SA token controller needs a moment before the kube-agent RBAC applies cleanly.
    if "kube-agent-rbac.yaml" in $config.apply_order {
        print "  waiting 5s for the SA token controller..."
        sleep 5sec
        apply_file $output_dir "kube-agent-rbac.yaml"
    }
    ok "manifests applied"
}

def step_forwards [forwards: list] {
    let missing = ($forwards | where {|f| not ({ ^pgrep -f $"svc/($f.svc)" } | cmd_ok) })
    if ($missing | is-empty) {
        ok "required port-forwards already running"
        return
    }
    let names = ($missing | get name)
    ^just port-forwards-up ...$names
    if $env.LAST_EXIT_CODE != 0 { fail "just port-forwards-up failed" }
    sleep 3sec
    ok $"started: ($names | str join ', ')"
}

def step_admin [username: string, mail: string] {
    let dir = (admin_dir)
    let cluster_ca = (^kubectl -n $POLAR get configmap polar-ca -o "jsonpath={.data.ca\\.pem}" | str trim)
    let ca_file = ($dir | path join "ca.pem")
    if ($ca_file | path exists) and ((open --raw $ca_file | str trim) != $cluster_ca) {
        fail $"($dir) holds material from a previous cluster. Move it aside and re-run: mv ($dir) ($dir).old"
    }

    if ($dir | path join "bootstrap-admin.crt" | path exists) {
        ok "bootstrap admin already exists"
    } else {
        ^just admin-bootstrap
        if $env.LAST_EXIT_CODE != 0 { fail "just admin-bootstrap failed" }
    }

    if ($dir | path join $"($username).crt" | path exists) {
        ok $"($username) already has a certificate, skipping provision and claim"
    } else {
        ^just admin-provision $username $mail
        if $env.LAST_EXIT_CODE != 0 { fail "just admin-provision failed" }
        ^just admin-claim $username $mail
        if $env.LAST_EXIT_CODE != 0 { fail "just admin-claim failed" }
    }
}

def step_browser [username: string] {
    pause "Close Librewolf so its certificate store is not locked."
    ^just user-cert-install $username
    if $env.LAST_EXIT_CODE != 0 { fail "just user-cert-install failed" }

    let neo4j_secret = (^kubectl get secret -n $GRAPH neo4j-secret -o "jsonpath={.data.secret}" | ^base64 -d)
    let neo4j_password = ($neo4j_secret | split row "/" | skip 1 | str join "/")
    print ""
    print "  Open https://localhost:7473 in Librewolf and pick your certificate."
    print "  Log in as neo4j / neo4j and set the new password to:"
    print $"    ($neo4j_password)"
    print "  (A wrong password retried repeatedly trips Neo4j's failed-login rate limit.)"
    pause "Change the Neo4j password in the browser."
}

def main [
    target: string          # Target name, e.g. local-minimal
    --dev                   # Also build and load images (local development cluster)
    --user: string          # Username for the first admin account
    --email: string         # Email for the first admin account
] {
    if not ("infra/render.nu" | path exists) {
        fail "run this from the repo root (infra/render.nu not found)"
    }
    let target_file = ($env.PWD | path join $"infra/targets/($target)/target.nu")
    if not ($target_file | path exists) {
        fail $"unknown target ($target): no target.nu at ($target_file)"
    }
    let config = (^$nu.current-exe $target_file | from nuon)

    let plan = (build_plan $config $dev)
    let enabled = ($plan | where on)
    let waits = (build_waits $config)
    let forwards = (build_forwards $config)
    print_plan $target $plan

    # Ask for what the plan needs up front, so nothing prompts mid-run.
    let needs_admin = ($enabled | any {|s| $s.id == "admin" })
    let username = if not $needs_admin { "" } else if $user == null { input "  username for the first admin: " } else { $user }
    let mail = if not $needs_admin { "" } else if $email == null { input "  email for the first admin: " } else { $email }

    let total = ($enabled | length)
    for it in ($enabled | enumerate) {
        let s = $it.item
        print ""
        print $"[($it.index + 1)/($total)] ($s.title)"
        match $s.id {
            "preflight" => { step_preflight }
            "render" => { step_render $target }
            "images" => { step_images $config }
            "namespaces" => { apply_file $config.output_dir "namespaces.yaml" }
            "ca" => { step_ca }
            "infra" => { step_infra $config }
            "pods" => { wait_pods $waits 10min }
            "forwards" => { step_forwards $forwards }
            "admin" => { step_admin $username $mail }
            "browser" => { step_browser $username }
        }
    }

    print ""
    print $"=== Target ($target) is up ==="
    ^kubectl -n $POLAR get pods
    note "other services (keycloak, ace, jaeger) are not forwarded here; run: just port-forwards-up"
}
