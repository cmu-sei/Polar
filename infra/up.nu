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

# infra/up.nu
#
# Brings a Polar cluster up from a target, end to end. Run it from the repo
# root with KUBECONFIG pointing at the cluster:
#
#   nu infra/up.nu local-minimal --dev --user alice --email alice@example.org
#
# It is target-aware: what to do is derived from the target's own config (the
# charts it deploys, its apply order, whether it provisions the cluster CA), so
# a step that the target does not need is skipped, with the reason printed in
# the plan up front. infra/render.nu only renders; this script applies.
#
# Every step checks its own precondition, so after a failure or a pause you can
# re-run the same command and it resumes. It stops for a human only where a
# human is required: closing the browser for the certificate import, and the
# Neo4j password only if Neo4j still has its factory default (it is checked,
# and a cluster brought up by this script already has the agents' one).
#
# Steps (each enabled only when the target needs it):
#   preflight   cluster reachable, namespaces can be made, run from the repo root; a container runtime   always
#               (podman or docker) only when a step below starts a container here (the host registry for --dev, or a gitops target)
#   registry    apply the registry in the cluster (and the storage it needs), forward to it    cluster.registry_in_cluster
#   images      every image the target runs (its charts, rendered alone, and   --dev or --import-from
#               its catalog name them): copied from --import-from, built and
#               pushed with --dev; before the render, which asks the agents'
#               images (--scaffold, --check) and may pin them by digest
#   render      ask the agents' images for their scaffolds on the cluster,    always
#               then nu infra/render.nu <target>, and check it names only
#               the images prepared
#   namespaces  kubectl apply namespaces.yaml                             namespaces.yaml in apply_order
#   ca          polar-admin-grant bootstrap-ca                            cert-issuer deployed and cert_issuer_ca_configmap set
#   infra       target secrets.nu, then every manifest in apply order     always
#   pods        wait for the deployed core services                       any of cert-issuer, cassini, credentials-agent, neo4j
#   forwards    just port-forwards-up <only what is needed>               credentials-agent (observer) or neo4j
#   admin       admin-bootstrap / admin-provision / admin-claim           credentials-agent deployed
#   seed        sign the first policy and push it to polar-schedules      credentials-agent, polar-git-server and polar-scheduler-observer deployed
#   browser     user-cert-install, then check the Neo4j password          credentials-agent and neo4j deployed

const POLAR = "polar"
const GRAPH = "polar-graph"

use images.nu
use secrets-lib.nu [existing_required_secrets, secrets_check]
use retry.nu [with_retries]
use admin.nu [aside_path, is_another_clusters, move_aside]
use browser.nu [browser_expected, browser_found, browser_problems, neo4j_answer, neo4j_probe, nss_db, password_verdict, probe_until_answered]
use forwards.nu [ensure_observer_forward, forward_live, stop_stale]
use egress.nu [egress_report, kube_probe, probe_url]
use seed.nu
use cluster-inputs.nu
use tools.nu
use runtime.nu [container_runtime]
use plan.nu [build_forwards, build_plan, deployed_charts, needs_runtime]
use agent-pods.nu [delete_stale_namespaces]
use cluster-registry.nu
use registry.nu [node_registry]
use apply.nu [any_pod_ready, changed_configmaps, changed_lines, config_users, gate_after, job_verdict, jobs_in, stale_images, stale_pod_images, stale_pod_report, workload_needs_restart]

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

# The registry as this machine reaches it. The cluster names it differently (POLAR_NODE_REGISTRY, infra/registry.nu; 10.0.2.2:5000 by default, the host as the
# node sees it), so an image reference is looked up here by its path and tag.
def registry_host [] { $env.POLAR_REGISTRY? | default "localhost:5000" }

# The digest the registry serves for an image as the cluster names it ("10.0.2.2:5000/x:latest"), or null
# when it cannot say. The same digest a pod reports as its image once it has pulled it.
def served_digest [image: string] {
    let parsed = ($image | parse --regex '^[^/]+/(?<name>.+):(?<tag>[^:/]+)$')
    if ($parsed | is-empty) { return null }
    let url = $"http://(registry_host)/v2/($parsed.0.name)/manifests/($parsed.0.tag)"
    let r = (do { ^curl -sI -m 5 -H "Accept: application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.manifest.v1+json" $url } | complete)
    if $r.exit_code != 0 { return null }
    let line = ($r.stdout | lines | where {|l| ($l | str lowercase | str starts-with "docker-content-digest:") })
    if ($line | is-empty) { null } else { $line | first | str replace --regex '(?i)^docker-content-digest:\s*' '' | str trim }
}

# The images a Job's pods are running, init containers too (polar-init is rebuilt like the rest): the image
# as the pod names it, and the digest it pulled.
def running_images [ns: string, job: string] {
    let r = (do { ^kubectl -n $ns get pods -l $"job-name=($job)" -o json } | complete)
    if $r.exit_code != 0 { return [] }
    $r.stdout
    | from json
    | get items
    | where {|p| (($p.metadata?.deletionTimestamp? | default null) == null) and (($p.status?.phase? | default "") == "Running") }
    | each {|p|
        (($p.status?.containerStatuses? | default []) ++ ($p.status?.initContainerStatuses? | default []))
        | where {|c| ($c.imageID? | default "") != "" }
        | each {|c| { image: $c.image, digest: ($c.imageID | split row "@" | last) } }
    }
    | flatten
}

# A Deployment's or StatefulSet's `metadata.generation`, which goes up only when its spec changes, or null when
# there is no such object.
def generation_of [ns: string, kind: string, name: string] {
    let r = (do { ^kubectl -n $ns get $"($kind)/($name)" -o jsonpath="{.metadata.generation}" } | complete)
    let text = ($r.stdout | str trim)
    if $r.exit_code != 0 or ($text | is-empty) { null } else { $text | into int }
}

# Which of these Jobs run an image the registry no longer serves under that tag (apply.nu's stale_images).
def stale_for [jobs: list] {
    let running = ($jobs | each {|j| running_images $j.ns $j.name } | flatten)
    if ($running | is-empty) { return [] }
    let served = ($running | get image | uniq | each {|i| { image: $i, digest: (served_digest $i) } } | where digest != null)
    stale_images $running $served
}

# kubectl apply, deleting first when the manifest holds a Job (their pod templates are immutable). A manifest
# whose Jobs are unchanged and ready is left alone: restarting what is already running drops every session its
# peers hold. It is restarted when the manifest differs, when a Job's image is no longer what the registry
# serves (images keep their tags, so an unchanged manifest would leave the old code running, and only a Job whose
# own image changed has anything to restart for), or when a ConfigMap it uses was just changed (it reads it at
# start). A Deployment or StatefulSet that uses a ConfigMap that just changed is rolled the same way.
# `changed` is the ConfigMaps changed by the manifests applied before this one; returns the ones this one changed.
# `quiet` (the seed's re-apply) says nothing of a manifest that changed nothing.
def apply_file [output_dir: string, filename: string, changed: list<string> = [], --quiet] {
    let manifest = ($output_dir | path join $filename)
    if not ($manifest | path exists) {
        note $"skipping ($filename), not found"
        return { changed: [] }
    }
    let users = (config_users (open --raw $manifest))
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
        let stale = if $diff.exit_code == 0 { stale_for $jobs } else { [] }
        let changed_config = ($users | where kind == "Job" | get configmaps | flatten | where {|c| $c in $changed } | uniq)
        let verdict = (job_verdict $diff.exit_code $jobs $stale $changed_config)
        if not $verdict.restart {
            if not $quiet { ok $"($filename): left running, ($verdict.reason)" }
            return { changed: [] }
        }
        note $"($filename): restarting its Jobs, ($verdict.reason)"
    }
    if not $quiet or $has_job { print $"  applying ($filename)..." }
    if $has_job {
        ^kubectl delete --ignore-not-found -f $manifest
    }
    let rollers = ($users | where kind in ["Deployment" "StatefulSet"])
    # Each in a record: `each` drops a null (a Deployment that does not exist yet has no generation), which would
    # shift every later one out of its place.
    let before = ($rollers | each {|u| { generation: (generation_of $u.ns ($u.kind | str lowercase) $u.name) } })
    let applied = (do { ^kubectl apply -f $manifest } | complete)
    let shown = (changed_lines $applied.stdout)
    if ($shown | is-not-empty) {
        if $quiet and not $has_job { print $"  applying ($filename)..." }
        print ($shown | str join "\n")
    } else if not $quiet {
        print "  unchanged"
    }
    if $applied.exit_code != 0 {
        print -e ($applied.stderr | str trim)
        fail $"kubectl apply failed for ($filename)"
    }
    let now_changed = (changed_configmaps $applied.stdout)
    # A Deployment or StatefulSet is not restarted by its ConfigMap changing, and it reads the ConfigMap when its
    # pods start. If its own spec changed it is rolling already and is not rolled twice; whether it did is its
    # generation, not what kubectl printed, since `configured` is printed for more than a changed spec.
    for it in ($rollers | enumerate) {
        let u = $it.item
        let kind = ($u.kind | str lowercase)
        let hit = ($u.configmaps | where {|c| $c in ($changed ++ $now_changed) })
        let after = (generation_of $u.ns $kind $u.name)
        if (workload_needs_restart ($before | get $it.index | get generation) $after $hit) {
            note $"($kind)/($u.name): restarting it, its ConfigMap changed, ($hit | str join ', ')"
            ^kubectl -n $u.ns rollout restart $"($kind)/($u.name)"
        }
    }
    { changed: $now_changed }
}

# True when at least one pod matching the selector is Running with every container ready, and
# not on its way out.
def pod_ready [ns: string, selector: string] {
    let r = (do { ^kubectl -n $ns get pods -l $selector -o json } | complete)
    if $r.exit_code != 0 { return false }
    any_pod_ready ($r.stdout | from json | get items)
}

# Says so when a running pod's image is not the one the registry serves now for its name. A node that kept the first copy it pulled of a
# tag that moved runs old code, and what it does looks like a bug in the new code. Polar pulls such tags Always, so this means the node
# is older than that, or something overrode it. A warning and not a failure: a cluster may be meant to run what it runs.
def report_stale_images [] {
    let r = (do { ^kubectl get pods -A -o json } | complete)
    if $r.exit_code != 0 { return }
    let pods = ($r.stdout | from json | get items)
    let images = ($pods
        | each {|p| (($p.status?.containerStatuses? | default []) ++ ($p.status?.initContainerStatuses? | default [])) | each {|c| $c.image? | default "" } }
        | flatten
        | uniq
        | where {|i| ($i != "") and not ($i | str contains "@") })
    let served = ($images | each {|i| { image: $i, digest: (served_digest $i) } } | where digest != null)
    let stale = (stale_pod_images $pods $served)
    if ($stale | is-empty) { return }
    print ""
    for l in (stale_pod_report $stale) { note $l }
    print ""
}

def wait_pods [waits: list, timeout: duration] {
    let deadline = ((date now) + $timeout)
    mut pending = ($waits | where kind == "pods")
    loop {
        $pending = ($pending | where {|w| not (pod_ready $w.ns $w.ref) })
        if ($pending | is-empty) { break }
        if (date now) > $deadline {
            report_stale_images
            fail $"timed out waiting for pods: ($pending | get label | str join ', ')"
        }
        print $"  waiting for: ($pending | get label | str join ', ')"
        sleep 5sec
    }
    for w in ($waits | where kind == "statefulset") {
        print $"  waiting for statefulset ($w.label)..."
        ^kubectl -n $w.ns rollout status $w.ref --timeout=600s
        if $env.LAST_EXIT_CODE != 0 {
            report_stale_images
            fail $"statefulset ($w.label) did not become ready"
        }
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

def step_preflight [config: record, runtime_needed: bool] {
    if not ("infra/render.nu" | path exists) {
        fail "run this from the repo root (infra/render.nu not found)"
    }
    if ($env.KUBECONFIG? | default "" | is-empty) {
        fail "KUBECONFIG is not set (point it at your cluster's kubeconfig, e.g. export KUBECONFIG=<path to kubeconfig>)"
    }
    let nodes = (do { ^kubectl get nodes -o name } | complete)
    if $nodes.exit_code != 0 {
        fail $"cluster not reachable: ($nodes.stderr | str trim)"
    }
    ok $"cluster reachable, ($nodes.stdout | lines | length) node\(s\)"
    if $runtime_needed {
        let rt = (try { container_runtime } catch {|e| fail $e.msg })
        ok $"container runtime: ($rt)"
    } else {
        ok "no container runtime needed: the agents are asked about themselves on the cluster"
    }
    # The render asks each agent's image for its scaffold in a scratch namespace of its own, deleted afterwards.
    let can = (do { ^kubectl auth can-i create namespaces } | complete)
    if ($can.stdout | str trim) != "yes" {
        fail "this account cannot create namespaces, which the render needs: it asks the agents for their scaffolds in a scratch namespace, deleted afterwards"
    }
    # A gitops target's layers are published as commits signed with gpgsm; without it the run would stop at the publish, after the
    # images and the render.
    if ($config.gitops? | default false) and (which gpgsm | is-empty) {
        fail "this target is published as signed commits, which needs gpgsm (GnuPG) on the PATH: it is in the dev container, or `nix shell nixpkgs#gnupg`"
    }
    # kubectl diff runs `diff`. Without it no manifest can be compared with the cluster, so every Job is restarted when applied.
    if (which diff | is-empty) { note "no `diff` on PATH: kubectl diff cannot compare manifests with the cluster, so every Job applied is restarted (install diffutils)" }
    let stale = (delete_stale_namespaces)
    if ($stale | is-not-empty) { note $"deleted scratch namespace\(s\) an interrupted run left behind: ($stale | str join ', ')" }

    # The secrets step needs the SOPS files to decrypt unless the secrets are already there. Found out here, in
    # seconds, and not after the images are built: a shell without the dev shell's SOPS variables was told so
    # only at the secrets step, after the whole build.
    if ($config.target_dir | path join "secrets.nu" | path exists) {
        let checked = (secrets_check $config.target_dir (existing_required_secrets))
        if ($checked.problems | is-not-empty) {
            for p in $checked.problems { print -e $"  FAILED: ($p)" }
            fail "the secrets step could not run; fix the above (the dev shell sets the SOPS variables), then run this again"
        }
        ok (if ($checked.failures | is-empty) { "secrets: the SOPS files decrypt" } else { "secrets: all the required secrets already exist" })
    }
}

# Identifies the cluster this machine is pointed at, by its CA (the ConfigMap `bootstrap-ca` makes), or null when it has none yet.
def cluster_fingerprint [] {
    let r = (do { ^kubectl -n $POLAR get configmap polar-ca -o 'jsonpath={.data.ca\.pem}' } | complete)
    if $r.exit_code != 0 or ($r.stdout | str trim | is-empty) { null } else { $r.stdout | hash sha256 }
}

def cluster_inputs_path [target: string] { cluster-inputs path_of ($env.PWD | path join "infra") $target }

# What this machine kept of another cluster's inputs (its administrators) is not this cluster's: discard it before it is rendered.
def discard_other_clusters_inputs [target: string] {
    let path = (cluster_inputs_path $target)
    if not ($path | path exists) { return }
    let held = (cluster-inputs read_inputs $path)
    if (cluster-inputs reconcile $held (cluster_fingerprint)) != $held {
        rm $path
        note $"discarded ($path | path relative-to $env.PWD): it was made for another cluster"
    }
}

# The render reads the agents' scaffolds from a directory, asked of their images on the cluster just before (the ones cached
# under their image's digest are not asked again), so render.nu itself never touches a cluster.
# `quiet` (the seed's render, after the first) keeps the render's output back, unless it fails.
def step_render [target: string, prepared: any = null, config: any = null, --quiet] {
    discard_other_clusters_inputs $target
    let scaffolds = (mktemp --directory)
    let collected = (do { ^$nu.current-exe infra/resolve.nu collect-scaffolds $scaffolds --target $target } | complete)
    if not $quiet or $collected.exit_code != 0 { print -n $collected.stdout; print -e -n $collected.stderr }
    if $collected.exit_code != 0 {
        rm -rf $scaffolds
        fail "the agents could not be asked for their scaffolds (above)"
    }
    let code = if $quiet {
        let r = (with-env { POLAR_SCAFFOLD_DIR: $scaffolds } { do { ^$nu.current-exe infra/render.nu $target } | complete })
        if $r.exit_code != 0 { print -e $r.stdout $r.stderr } else { ok "rendered again (its output is shown only if it fails)" }
        $r.exit_code
    } else {
        with-env { POLAR_SCAFFOLD_DIR: $scaffolds } { ^$nu.current-exe infra/render.nu $target }
        $env.LAST_EXIT_CODE
    }
    rm -rf $scaffolds
    if $code != 0 { fail "render failed" }
    if $prepared != null and $config != null {
        let files = ($config.apply_order | each {|f| $config.output_dir | path join $f } | where {|f| $f | path exists })
        let needed = (try { images needed $files | get image } catch {|e| fail $e.msg })
        let missing = (images unprepared $needed $prepared)
        if ($missing | is-not-empty) {
            fail $"the render names images the images step did not prepare: ($missing | str join ', '). Its list \(render.nu --charts-into, resolve.nu images\) and the render disagree"
        }
    }
}

# The images the target runs, worked out before the render, from what the render is given: the charts' manifests, rendered
# alone into a scratch directory (no chart needs an agent's scaffold for its images), and the catalog's workloads, as their
# resolved specs name them (resolve.nu images). Their rows of infra/images.nuon.
def target_images [target: string] {
    let charts = (mktemp --directory)
    let r = (do { ^$nu.current-exe infra/render.nu $target --charts-into $charts } | complete)
    if $r.exit_code != 0 {
        rm -rf $charts
        print -e $r.stdout $r.stderr
        fail "rendering the charts, for the images they name, failed (above)"
    }
    let catalog = (^$nu.current-exe infra/resolve.nu images --target $target | from json)
    let rows = (try { images needed (glob ($charts | path join "*.yaml")) $catalog } catch {|e| rm -rf $charts; fail $e.msg })
    rm -rf $charts
    $rows
}

# Every image the target runs, in the registry before the render, each handled once: copied from where they are published
# (--import-from; a third-party image from its own registry), then built from this checkout and pushed (--dev). The render asks
# the agents' images for their scaffolds, and a target that pins its images by digest names the ones just pushed. Returns the
# images it prepared, which the render is checked against.
def step_images [target: string, in_cluster: bool, dev: bool, import_from: string] {
    let rows = (target_images $target)
    print $"  ($rows | length) image\(s\) this target runs"
    if ($import_from | is-not-empty) {
        let published = ($rows | where kind != "mirror" | get image)
        ^$nu.current-exe infra/images.nu import ...$published --from $import_from --skip-mirrors
        if $env.LAST_EXIT_CODE != 0 { fail $"importing the images from ($import_from) failed \(above\)" }
        if not $dev {
            try { images sync ($rows | where kind == "mirror") } catch {|e| fail $e.msg }
        }
    }
    if $dev {
        if not $in_cluster { ^just registry-host-up }
        try { images sync $rows } catch {|e| fail $e.msg }
    }
    ok (if $dev { "images built and pushed" } else { $"images copied from ($import_from)" })
    $rows | get image
}

def step_ca [] {
    let have_ca = ({ ^kubectl -n $POLAR get secret cert-issuer-ca -o name } | cmd_ok)
    if $have_ca {
        ok "secret/cert-issuer-ca already exists, skipping bootstrap-ca"
    } else {
        let t = (tools command "polar-admin-grant")
        ^($t | first) ...($t | skip 1) bootstrap-ca
        if $env.LAST_EXIT_CODE != 0 { fail "bootstrap-ca failed" }
        ok "cluster CA created"
    }
}

# `quiet` keeps what secrets.nu prints back, unless it fails.
def run_secrets [config: record, --quiet] {
    let secrets_script = ($config.target_dir | path join "secrets.nu")
    if ($secrets_script | path exists) {
        if $quiet {
            let r = (do { ^$nu.current-exe $secrets_script $config.target_dir $config.repo_root } | complete)
            if $r.exit_code != 0 { print -e $r.stdout $r.stderr; fail "secrets.nu failed" }
        } else {
            print "  creating secrets..."
            ^$nu.current-exe $secrets_script $config.target_dir $config.repo_root
            if $env.LAST_EXIT_CODE != 0 { fail "secrets.nu failed" }
        }
    }
}

# A gitops target's secrets are made here and never committed: Flux does not own them and never prunes them.
def step_secrets [config: record] {
    run_secrets $config
    ok "secrets created"
}

# The scripts a gitops target runs in place of the apply loop. Each stops the run on failure with its own message.
def run_script [args: list<string>] {
    ^$nu.current-exe ...$args
    if $env.LAST_EXIT_CODE != 0 { fail $"($args | skip 1 | first 2 | str join ' ') failed" }
}

def step_deploy_host [target: string] { run_script [infra/deploy-repo.nu ensure $target] }

def step_publish [target: string, allow_dirty: bool] {
    run_script ([infra/deploy-repo.nu publish $target] ++ (if $allow_dirty { ["--allow-dirty"] } else { [] }))
}

def step_flux [target: string] { run_script [infra/flux.nu install $target] }
def step_flux_wait [target: string] { run_script [infra/flux.nu wait $target] }

# `quiet` (the seed's re-apply, after the first) prints only what changed and what restarts.
def step_infra [config: record, --quiet] {
    let output_dir = $config.output_dir
    run_secrets $config --quiet=$quiet
    mut changed: list<string> = []
    for filename in ($config.apply_order | skip 1) {
        let applied = (apply_file $output_dir $filename $changed --quiet=$quiet)
        $changed = ($changed ++ $applied.changed | uniq)
        # What the next manifests connect to must be up before they start: a client that comes
        # up first attaches to a broker that is about to go away, and loses it.
        let gate = (gate_after $filename)
        if $gate != null and ($output_dir | path join $filename | path exists) {
            if not $quiet { print $"  waiting for ($gate.label) before applying what follows..." }
            wait_pods [{ kind: "pods", ns: $gate.ns, ref: $gate.ref, label: $gate.label }] 5min
        }
    }
    ok "manifests applied"
}

def step_forwards [forwards: list] {
    # A forward left from another cluster's run holds the port and reaches nothing here: stopped, and started again.
    for f in $forwards {
        let stopped = (stop_stale $f.ns $f.svc)
        if $stopped > 0 { note $"stopped a port-forward for ($f.name) left from another cluster" }
    }
    let missing = ($forwards | where {|f| not (forward_live $f.ns $f.svc) })
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
    ensure_observer_forward
    let dir = (admin_dir)
    let cluster_ca = (^kubectl -n $POLAR get configmap polar-ca -o "jsonpath={.data.ca\\.pem}" | str trim)
    let ca_file = ($dir | path join "ca.pem")
    # The CA stored there is not this cluster's, so what is there was issued by another cluster and is of no use
    # here. What is in it is moved aside, not deleted, under a name that cannot overwrite an earlier one.
    let stored_ca = if ($ca_file | path exists) { open --raw $ca_file } else { null }
    if (is_another_clusters $stored_ca $cluster_ca) {
        let aside = (aside_path $dir (date now | format date "%Y%m%d-%H%M%S") (glob $"($dir).old*"))
        move_aside $dir $aside
        note $"($dir) held certificates from another cluster \(its CA is not this cluster's\); moved to ($aside), nothing deleted"
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

# The browser holds this cluster's CA and the user's certificate, and Neo4j's password is the one the agents use.
# Each is looked at first, read-only; a person is asked only for what is missing, and told which.
def step_browser [username: string] {
    let dir = (admin_dir)

    # In the in-cluster dev container (bin/polar-dev) there is no browser here: the certificate is imported on the user's
    # machine, by the same script `just user-cert-install` runs.
    if ($env.POLAR_DEV_POD? | default "") == "1" {
        note "this is the in-cluster dev container: the browser is on your own machine"
        pause $"On your machine, close Librewolf and run: bin/polar-dev cert ($username)   \(it copies the certificate out and imports it\), then bin/polar-dev forward   \(Neo4j and ACE for the browser there\)"
    } else {
        let db = (nss_db)
        let browser_problems = if $db == null {
            ["no Librewolf profile with a certificate database was found under ~/.librewolf or ~/.config/librewolf (set POLAR_NSS_DB to its directory)"]
        } else {
            browser_problems (browser_found $db $username) (browser_expected $dir $username) $username
        }
        if ($browser_problems | is-empty) {
            ok $"the browser trusts this cluster's CA and holds ($username)'s current certificate"
        } else {
            for p in $browser_problems { note $p }
            pause "Close Librewolf so its certificate store is not locked."
            ^just user-cert-install $username
            if $env.LAST_EXIT_CODE != 0 { fail "just user-cert-install failed" }
        }
    }

    # The password the agents use is the Secret's; whether Neo4j has it tells whether the first login happened.
    let agents_password = (^kubectl -n $POLAR get secret polar-graph-pw -o "jsonpath={.data.secret}" | ^base64 -d)
    # Neo4j that was just (re)started answers nothing for up to a minute or so: wait for it rather than fail.
    let first = (probe_until_answered 30 4sec {|| neo4j_probe $dir $username $agents_password })
    let with_secret = (neo4j_answer $first.code $first.body)
    # Only if the Secret's password is refused is the factory default tried, so a Neo4j still on it is recognised
    # (it refuses the Secret's password) and one with a password of its own gets no more failed logins than that.
    let with_default = if $with_secret == "refused" {
        let second = (neo4j_probe $dir $username "neo4j")
        neo4j_answer $second.code $second.body
    } else { null }
    match (password_verdict $with_secret $with_default) {
        "set" => { ok "Neo4j's password is already the one the agents use" }
        "default" => {
            let neo4j_secret = (^kubectl get secret -n $GRAPH neo4j-secret -o "jsonpath={.data.secret}" | ^base64 -d)
            let neo4j_password = ($neo4j_secret | split row "/" | skip 1 | str join "/")
            print ""
            print "  Open https://localhost:7473 in Librewolf and pick your certificate."
            print "  Log in as neo4j / neo4j and set the new password to:"
            print $"    ($neo4j_password)"
            print "  (A wrong password retried repeatedly trips Neo4j's failed-login rate limit.)"
            pause "Change the Neo4j password in the browser."
        }
        "different" => {
            note "Neo4j has a password other than the one in the polar-graph-pw Secret, so the agents cannot log in. Either set the Secret to the password it has, or change Neo4j's to the Secret's; this is not the default password, so there is no first login to do."
        }
        "unreachable" => {
            fail "Neo4j did not answer at https://localhost:7473 in two minutes, so its password cannot be checked: start its port-forward (just port-forwards-up neo4j) and run this again"
        }
        _ => {
            note $"Neo4j answered http ($first.code) to the password check, which is neither success nor a request to change the password; not asking for anything"
        }
    }
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

def step_seed [target: string, config: record, username: string, mail: string, allow_dirty: bool] {
    wait_pods (seed_waits) 10min
    let dir = (admin_dir)
    let root = $env.PWD
    let work = (mktemp -d)
    let repo = ($work | path join "repo")

    let san = (^openssl x509 -in ($dir | path join $"($username).crt") -noout -ext subjectAltName)
    let missing = (seed lacks $san (seed documents))
    if ($missing | is-not-empty) { fail (seed lacks_hint $username $missing) }

    # The git server's administrator is this cluster's first account, which did not exist when the layers were rendered. It is an
    # input like any other: written to the cluster's inputs, rendered, published signed, and recorded in the provenance, which
    # names the file and its digest. Flux restarts the git server's Job because its ConfigMap's fingerprint changes.
    let identity = (seed first_dns_name $san)
    if $identity != null {
        let cluster = (cluster_fingerprint)
        if $cluster == null {
            note $"this cluster has no CA to tie the administrator ($identity) to; not recording it"
        } else {
            let path = (cluster_inputs_path $target)
            let recorded = (cluster-inputs with_admin (cluster-inputs read_inputs $path) $identity $cluster)
            if $recorded.changed {
                $recorded.inputs | to nuon --indent 2 | save --force $path
                note $"recorded this cluster's administrator ($identity) in ($path | path relative-to $env.PWD); applying it to the git server"
                step_render $target --quiet
                if ($config.gitops? | default false) {
                    step_publish $target $allow_dirty
                    step_flux_wait $target
                } else {
                    step_infra $config --quiet
                }
                wait_pods (seed_waits) 10min
            }
        }
    }

    # Through a port-forward that may be reconnecting to a pod that was just replaced (every pod restarts
    # hourly, before its certificate expires), so a transport failure is tried again for a while; a refusal is not.
    let cloned = (with_retries 6 5sec {|n|
        let r = (do { seed_git $username [clone (seed repo_url) $repo] } | complete)
        if $r.exit_code != 0 {
            # Its error, when every try fails or the server refuses, is in the failure below.
            if not ($r.stderr | str contains "403") { note "the git server does not answer yet (its pod, or the port-forward to it, was just replaced): trying again" }
            rm -rf $repo
        }
        seed clone_outcome $r.exit_code $r.stderr
    })
    if not $cloned.ok {
        let identity = (seed first_dns_name $san)
        if ($cloned.detail | str contains "403") and ($identity != null) {
            fail (seed refused_hint $identity $target)
        }
        fail $"cannot clone polar-schedules after ($cloned.tries) tries: ($cloned.detail)"
    }

    let existing = (^git -C $repo ls-files | lines)
    let todo = (seed missing (seed documents) $existing)
    let sign = (tools command "polar-sign")
    for d in $todo {
        note $"signing ($d.section) for ($d.agent), version ($d.version)"
        let signed = (seed body_json $d $root
            | ^($sign | first) ...($sign | skip 1) runtime --agent $d.agent --section $d.section --version $d.version --body-file - --user $username)
        if $env.LAST_EXIT_CODE != 0 { fail "polar-sign runtime failed" }
        let target_file = ($repo | path join $d.path)
        mkdir ($target_file | path dirname)
        $signed | save --force $target_file
    }
    if ($todo | is-empty) {
        ok "every seed document is already in polar-schedules"
    } else {
        ^git -C $repo add -A
        ^git -C $repo -c $"user.name=($username)" -c $"user.email=($mail)" commit -q -m $"feat\(seed\): the cluster's first ($todo | get section | str join ', ')"
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

# Whether a pod can reach the outside, said once the cluster is up. A note, never a failure: the cluster works
# offline, and a probe that cannot run is not a reason to stop.
def report_egress [] {
    let r = try { egress_report (probe_url) {|u| kube_probe $u } } catch {|e|
        { verdict: "error", lines: [$"could not check access to the outside: ($e.msg)"] }
    }
    for l in $r.lines { if $r.verdict == "ok" { ok $l } else { note $l } }
}

def main [
    target: string          # Target name, e.g. local-minimal
    --dev                   # Also build and load images (local development cluster)
    --user: string          # Username for the first admin account
    --email: string         # Email for the first admin account
    --allow-dirty           # Publish a gitops target from a working tree with uncommitted changes (the provenance records it)
    --import-from: string = ""  # Copy the published images into the registry first, e.g. docker.io/<account> (infra/images.nu import)
] {
    if not ("infra/render.nu" | path exists) {
        fail "run this from the repo root (infra/render.nu not found)"
    }
    let target_file = ($env.PWD | path join $"infra/targets/($target)/target.nu")
    if not ($target_file | path exists) {
        fail $"unknown target ($target): no target.nu at ($target_file)"
    }
    let config = (^$nu.current-exe $target_file | from nuon)

    # A target whose registry is in its cluster: everything this run pushes, and asks digests of, goes through the forward the
    # registry step starts (infra/cluster-registry.nu).
    let in_cluster_registry = (cluster-registry settings_of (cluster-registry cluster_of $target))
    let in_cluster = ($in_cluster_registry != null)
    if $in_cluster { $env.POLAR_REGISTRY = $"localhost:($in_cluster_registry.local_port)" }
    # The registry as this target's nodes name it, for every script this run starts (images.nu evicts a node's cached copy by it).
    $env.POLAR_NODE_REGISTRY = (node_registry $target)

    let plan = (build_plan $config $dev { in_cluster: $in_cluster, import_from: $import_from })
    let enabled = ($plan | where on)
    let waits = (build_waits $config)
    let forwards = (build_forwards $config)
    print_plan $target $plan $dev

    # Ask for what the plan needs up front, so nothing prompts mid-run.
    let needs_admin = ($enabled | any {|s| $s.id in ["admin" "seed"] })
    let username = if not $needs_admin { "" } else if $user == null { input "  username for the first admin: " } else { $user }
    let mail = if not $needs_admin { "" } else if $email == null { input "  email for the first admin: " } else { $email }

    let total = ($enabled | length)
    # The images the images step prepared, which the render's manifests must keep to (null when it did not run).
    mut prepared: any = null
    for it in ($enabled | enumerate) {
        let s = $it.item
        print ""
        print $"[($it.index + 1)/($total)] ($s.title)"
        match $s.id {
            "preflight" => { step_preflight $config (needs_runtime $plan) }
            "registry" => { cluster-registry ensure $target | ignore }
            "images" => { $prepared = (step_images $target $in_cluster $dev $import_from) }
            "render" => { step_render $target $prepared $config }
            "namespaces" => { apply_file $config.output_dir "namespaces.yaml" }
            "ca" => { step_ca }
            "infra" => { step_infra $config }
            "secrets" => { step_secrets $config }
            "deploy-host" => { step_deploy_host $target }
            "publish" => { step_publish $target $allow_dirty }
            "flux" => { step_flux $target }
            "flux-wait" => { step_flux_wait $target }
            "pods" => {
                wait_pods $waits 10min
                report_stale_images
            }
            "forwards" => { step_forwards $forwards }
            "admin" => { step_admin $username $mail }
            "browser" => { step_browser $username }
            "seed" => { step_seed $target $config $username $mail $allow_dirty }
        }
    }

    print ""
    print $"=== Target ($target) is up ==="
    ^kubectl -n $POLAR get pods
    report_egress
    if ($env.POLAR_DEV_POD? | default "") == "1" {
        note "for a browser on your own machine: bin/polar-dev forward there (Neo4j and ACE); just port-forwards-up there has every service"
    } else {
        note "other services (keycloak, ace, jaeger) are not forwarded here; run: just port-forwards-up"
    }
}
