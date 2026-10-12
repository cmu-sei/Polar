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
#   agent-images build and push the images of the agents that describe          --dev
#               themselves, before the render asks them (--scaffold, --check)
#   render      nu infra/render.nu <target>                               always
#   images      build and push only what the rendered manifests reference --dev
#   namespaces  kubectl apply namespaces.yaml                             namespaces.yaml in apply_order
#   ca          polar-admin-grant bootstrap-ca                            cert-issuer deployed and cert_issuer_ca_configmap set
#   infra       target secrets.nu, then every manifest in apply order     always
#   pods        wait for the deployed core services                       any of cert-issuer, cassini, credentials-agent, neo4j
#   forwards    just port-forwards-up <only what is needed>               credentials-agent (observer) or neo4j
#   admin       admin-bootstrap / admin-provision / admin-claim           credentials-agent deployed
#   seed        sign the first policy and push it to polar-schedules      credentials-agent, polar-git-server and polar-scheduler-observer deployed
#   browser     user-cert-install, then the Neo4j password                credentials-agent and neo4j deployed

const POLAR = "polar"
const GRAPH = "polar-graph"

use images.nu
use seed.nu
use plan.nu [build_forwards, build_plan, deployed_charts]
use apply.nu [any_pod_ready, gate_after, job_verdict, jobs_in]

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

# kubectl apply, deleting first when the manifest holds a Job (their pod templates are
# immutable). A manifest whose Jobs are unchanged and ready is left alone, unless the images
# were just rebuilt: a re-run should not restart what is already running, since that drops
# every session its peers hold (decided by apply.nu's job_verdict).
def apply_file [output_dir: string, filename: string, rebuilt: bool = false] {
    let manifest = ($output_dir | path join $filename)
    if not ($manifest | path exists) {
        note $"skipping ($filename), not found"
        return
    }
    let has_job = (open --raw $manifest
        | find --regex r#'(?m)^\s*["']?kind["']?\s*:\s*["']?Job["']?\s*,?\s*$'#
        | is-not-empty)
    if $has_job {
        let diff = (do { ^kubectl diff -f $manifest } | complete)
        let jobs = if $diff.exit_code == 0 {
            let got = (do { ^kubectl get -f $manifest -o json } | complete)
            if $got.exit_code == 0 {
                jobs_in ($got.stdout | from json)
                | each {|j| $j | insert ready (pod_ready $j.ns $"job-name=($j.name)") }
            } else { [] }
        } else { [] }
        let verdict = (job_verdict $diff.exit_code $jobs $rebuilt)
        if not $verdict.restart {
            ok $"($filename): left running, ($verdict.reason)"
            return
        }
        note $"($filename): restarting its Jobs, ($verdict.reason)"
    }
    print $"  applying ($filename)..."
    if $has_job {
        ^kubectl delete --ignore-not-found -f $manifest
    }
    ^kubectl apply -f $manifest
    if $env.LAST_EXIT_CODE != 0 { fail $"kubectl apply failed for ($filename)" }
}

# True when at least one pod matching the selector is Running with every container ready, and
# not on its way out.
def pod_ready [ns: string, selector: string] {
    let r = (do { ^kubectl -n $ns get pods -l $selector -o json } | complete)
    if $r.exit_code != 0 { return false }
    any_pod_ready ($r.stdout | from json | get items)
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

def print_plan [target: string, plan: list, dev: bool] {
    print $"=== Plan for target ($target) ==="
    for s in $plan {
        if $s.on {
            print $"  run   ($s.title)"
        } else {
            print $"  skip  ($s.title): ($s.why)"
        }
    }
    if $dev {
        print "  note  --dev rebuilds the images, which keep their tags, so every Job is restarted"
    } else {
        print "  note  a Job whose manifest is unchanged and whose pods are ready is left running;"
        print "        the others are restarted. Which, is decided and printed as each manifest is applied."
    }
    print "  note  cassini is applied first of the agents and waited on before anything after it"
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

# The agents that describe themselves are asked for their scaffold, and to check the document they
# will be given, when the catalog is rendered. They are asked in their built image, the artifact
# that deploys, so those images exist before the render. (The images step later builds the rest
# and finds these already built.)
def step_agent_images [] {
    ^just registry-up
    let names = (^$nu.current-exe infra/resolve.nu document-images | from json)
    print $"  ($names | length) agent image\(s\) the render asks: ($names | str join ', ')"
    try { images sync (images named $names) } catch {|e| fail $e.msg }
    ok "agent images built and pushed"
}

def step_render [target: string] {
    with-env { POLAR_AGENT_SOURCE: "images" } {
        ^$nu.current-exe infra/render.nu $target
    }
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

def step_infra [config: record, rebuilt: bool] {
    let output_dir = $config.output_dir
    let secrets_script = ($config.target_dir | path join "secrets.nu")
    if ($secrets_script | path exists) {
        print "  creating secrets..."
        ^$nu.current-exe $secrets_script $config.target_dir $config.repo_root
        if $env.LAST_EXIT_CODE != 0 { fail "secrets.nu failed" }
    }
    for filename in ($config.apply_order | skip 1) {
        apply_file $output_dir $filename $rebuilt
        # What the next manifests connect to must be up before they start: a client that comes
        # up first attaches to a broker that is about to go away, and loses it.
        let gate = (gate_after $filename)
        if $gate != null and ($output_dir | path join $filename | path exists) {
            print $"  waiting for ($gate.label) before applying what follows..."
            wait_pods [{ kind: "pods", ns: $gate.ns, ref: $gate.ref, label: $gate.label }] 5min
        }
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

# What `just admin-provision` leaves for the seed: the signer's certificate must be one the git server
# accepts as an administrator, and the credentials agent must be up to take the policy.
def seed_waits [] {
    [
        { kind: "pods", ns: $POLAR, ref: "job-name=polar-git-server", label: "polar-git-server" }
        { kind: "pods", ns: $POLAR, ref: "job-name=polar-scheduler-observer", label: "polar-scheduler-observer" }
        { kind: "pods", ns: $POLAR, ref: "job-name=polar-scheduler", label: "polar-scheduler" }
        { kind: "pods", ns: $POLAR, ref: "job-name=credentials-agent", label: "credentials-agent" }
    ]
}

# git, for one command, presenting the user's Polar certificate (and not the environment's CA).
def seed_git [username: string, args: list<string>] {
    let config = (seed git_config $username (admin_dir))
    ^env -u GIT_SSL_CAINFO -u SSL_CERT_FILE git ...$config ...$args
}

def step_seed [target: string, username: string, mail: string] {
    wait_pods (seed_waits) 10min
    let dir = (admin_dir)
    let root = $env.PWD
    let work = (mktemp -d)
    let repo = ($work | path join "repo")

    let san = (^openssl x509 -in ($dir | path join $"($username).crt") -noout -ext subjectAltName)
    let missing = (seed lacks $san (seed documents))
    if ($missing | is-not-empty) { fail (seed lacks_hint $username $missing) }

    let cloned = (do { seed_git $username [clone (seed repo_url) $repo] } | complete)
    if $cloned.exit_code != 0 {
        let identity = (seed first_dns_name $san)
        if ($cloned.stderr | str contains "403") and ($identity != null) {
            fail (seed refused_hint $identity $target)
        }
        fail $"cannot clone polar-schedules: ($cloned.stderr | str trim)"
    }

    let existing = (^git -C $repo ls-files | lines)
    let todo = (seed missing (seed documents) $existing)
    for d in $todo {
        note $"signing ($d.section) for ($d.agent), version ($d.version)"
        let signed = (seed body_json $d $root
            | ^cargo run -q --manifest-path src/agents/Cargo.toml -p polar-sign -- runtime --agent $d.agent --section $d.section --version $d.version --body-file - --user $username)
        if $env.LAST_EXIT_CODE != 0 { fail "polar-sign runtime failed" }
        let target_file = ($repo | path join $d.path)
        mkdir ($target_file | path dirname)
        $signed | save --force $target_file
    }
    if ($todo | is-empty) {
        ok "every seed document is already in polar-schedules"
    } else {
        ^git -C $repo add -A
        ^git -C $repo -c $"user.name=($username)" -c $"user.email=($mail)" commit -q -m $"Seed the cluster: ($todo | get section | str join ', ')"
        if $env.LAST_EXIT_CODE != 0 { fail "git commit failed" }
        let pushed = (do { seed_git $username [-C $repo push -q origin HEAD] } | complete)
        if $pushed.exit_code != 0 { fail $"cannot push to polar-schedules: ($pushed.stderr | str trim)" }
        ok $"pushed: ($todo | get path | str join ', ')"
    }
    rm -rf $work

    # The seed is part of the cluster being up: say so only when the credentials agent has the policy.
    let deadline = ((date now) + 3min)
    loop {
        let pod = (^kubectl -n $POLAR get pods -l job-name=credentials-agent --field-selector=status.phase=Running -o name | lines | first)
        let logs = (^kubectl -n $POLAR logs $pod --tail=5000 | complete | get stdout)
        if (seed authority_published $logs "policy") { break }
        if (date now) > $deadline {
            fail "the credentials agent has not published the policy after 3 minutes: kubectl -n polar logs <credentials-agent pod> shows why"
        }
        print "  waiting for the credentials agent to apply the policy..."
        sleep 5sec
    }
    ok "the credentials agent has applied and published the policy"
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
    print_plan $target $plan $dev

    # Ask for what the plan needs up front, so nothing prompts mid-run.
    let needs_admin = ($enabled | any {|s| $s.id in ["admin" "seed"] })
    let username = if not $needs_admin { "" } else if $user == null { input "  username for the first admin: " } else { $user }
    let mail = if not $needs_admin { "" } else if $email == null { input "  email for the first admin: " } else { $email }

    let total = ($enabled | length)
    for it in ($enabled | enumerate) {
        let s = $it.item
        print ""
        print $"[($it.index + 1)/($total)] ($s.title)"
        match $s.id {
            "preflight" => { step_preflight }
            "agent-images" => { step_agent_images }
            "render" => { step_render $target }
            "images" => { step_images $config }
            "namespaces" => { apply_file $config.output_dir "namespaces.yaml" }
            "ca" => { step_ca }
            "infra" => { step_infra $config $dev }
            "pods" => { wait_pods $waits 10min }
            "forwards" => { step_forwards $forwards }
            "admin" => { step_admin $username $mail }
            "browser" => { step_browser $username }
            "seed" => { step_seed $target $username $mail }
        }
    }

    print ""
    print $"=== Target ($target) is up ==="
    ^kubectl -n $POLAR get pods
    note "other services (keycloak, ace, jaeger) are not forwarded here; run: just port-forwards-up"
}
