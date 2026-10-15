#!/usr/bin/env nu
# infra/tests/resolve.nu
#
# Tests of what the resolver produces for the real catalog: the shutdown budget, the
# grace period derived from it, and the health settings the agent reads. The pure
# rules are in tests/rules.nu; these are about the wiring around them.
#
#   nu infra/tests/resolve.nu

use std/assert

const RESOLVER = (path self | path dirname | path dirname | path join "resolve.nu")

# What each catalog workload's code declares it needs to drain
# (with_shutdown_budget in its supervisor). The deployment must give at least this.
const BUDGETS = {
    kube-observer: 40
    kube-consumer: 45
    jira-observer: 40
    jira-processor: 40
    credentials-agent: 50
    credentials-agent-observer: 40
    cassini: 20
    oci-registry-resolver: 20
    git-repo-observer: 40
    git-repo-processor: 45
    git-repo-controller: 10
}

# The record the template takes, as the pipeline gets it.
def resolve [name: string] {
    ^nu $RESOLVER json $name | from json
}

# The target a test reads a workload under: local-minimal, unless that target leaves it out (`enabled: false`),
# in which case `local`, which runs the whole catalog.
def target_for [name: string] {
    if ($name in (^nu $RESOLVER names | from json)) { "local-minimal" } else { "local" }
}

# The record for a workload under another target, for what local-minimal does not run.
def resolve_for [target: string, name: string] {
    ^nu $RESOLVER json $name --target $target | from json
}

# The settings the rules read.
def settings [name: string] {
    ^nu $RESOLVER settings $name | from json
}

# What the agent will be given for a variable: from its environment, or, for an agent that reads
# a configuration document, from the document the resolver wrote.
def env_value [r: record, name: string, target: string = "local-minimal"] {
    if ($r.env | where name == "POLAR_CONFIG" | is-empty) {
        $r.env | where name == $name | get -o 0.value
    } else {
        let maps = (^nu $RESOLVER resources $r.name --target $target | from json | get config_maps)
        $maps | first | get content | from json | get sections | each {|s| $s.variables } | flatten
        | where name == $name | get -o 0.value
    }
}

const INFRA = (path self | path dirname | path dirname)

# Runs `body` with a throwaway target whose config.nuon is `config`, and removes it afterwards, so a test can
# say what a target would change (a workload's issuer, whether the floor is enforced).
def with_target [config: record, body: closure] {
    let name = $"zz-test-(random chars --length 8)"
    let dir = ($INFRA | path join "targets" $name)
    mkdir $dir
    $config | to nuon | save --force ($dir | path join "config.nuon")
    try {
        do $body $name
        rm -rf $dir
    } catch {|e|
        rm -rf $dir
        error make { msg: $e.msg }
    }
}

def init_env_value [r: record, name: string] {
    $r.init_env | where name == $name | get -o 0.value
}

def main [] {
    let tests = {
        "every workload's budget is in what the agent is given, as the code declared": {
            for w in ($BUDGETS | transpose name secs) {
                let t = (target_for $w.name)
                let r = (resolve_for $t $w.name)
                assert equal (env_value $r "POLAR_SHUTDOWN_BUDGET_SECS" $t) ($w.secs | into string) $"($w.name)"
            }
        }

        "the grace period is the budget plus the margin": {
            for w in ($BUDGETS | transpose name secs) {
                let r = (resolve_for (target_for $w.name) $w.name)
                assert equal $r.termination_grace_secs ($w.secs + 10) $"($w.name)"
            }
        }

        "the grace period is never set separately from the budget": {
            # It is derived, so a workload entry cannot make the two disagree.
            for w in ($BUDGETS | transpose name secs) {
                let t = (target_for $w.name)
                let r = (resolve_for $t $w.name)
                assert (($r.termination_grace_secs) > (env_value $r "POLAR_SHUTDOWN_BUDGET_SECS" $t | into int)) $"($w.name)"
            }
        }

        "the image an agent's check pulls is a row of images.nuon, and the crate it builds is named separately": {
            # The check pulls by the image's name; the crate (cargo -p) is named for the package. For a workload
            # whose crate is not named for its image, the two differ, and looking the package up in the images
            # table finds no row (cassini-broker, polar-scheduler-observer).
            let rows = (open ($INFRA | path join "images.nuon") | get image)
            let names = (^nu $RESOLVER names | from json)
            assert (($names | length) > 0) "the catalog lists workloads"
            for n in $names {
                let a = (^nu $RESOLVER agent-names $n | from json)
                assert ($a.image in $rows) $"($n): image '($a.image)' has no row in images.nuon"
            }
        }

        "a workload whose crate is not named for its image is checked as that crate, and pulled as that image": {
            let a = (^nu $RESOLVER agent-names cassini | from json)
            assert equal $a { image: "cassini", package: "cassini-broker" }
            let b = (^nu $RESOLVER agent-names polar-scheduler-observer | from json)
            assert equal $b { image: "scheduler-observer", package: "polar-scheduler-observer" }
            # One named for its image is both.
            let c = (^nu $RESOLVER agent-names kube-consumer | from json)
            assert equal $c.image $c.package
        }

        "a workload a target disables is left out of what is listed and rendered for that target, and only that target": {
            with_target { workloads: { kube-observer: { enabled: false } } } {|t|
                let names = (^nu $RESOLVER names --target $t | from json)
                assert (not ("kube-observer" in $names)) "the disabled workload was listed"
                assert ("kube-consumer" in $names) "a workload that is not disabled was dropped"
                assert ("kube-observer" in (^nu $RESOLVER names | from json)) "the default target lost it"
            }
        }

        "the images of a workload a target disables are not built for it": {
            with_target { workloads: { kube-observer: { enabled: false } } } {|t|
                let with = (^nu $RESOLVER document-images | from json)
                let without = (^nu $RESOLVER document-images --target $t | from json)
                assert ("kube-observer" in $with)
                assert (not ("kube-observer" in $without)) "its image would be built for nothing"
                assert ("kube-consumer" in $without)
            }
        }

        "resolving a workload the target disables says so, and does not pretend it is unknown": {
            with_target { workloads: { kube-observer: { enabled: false } } } {|t|
                let off = (do { ^nu $RESOLVER json kube-observer --target $t } | complete)
                assert ($off.exit_code != 0)
                assert ($off.stderr | str contains "not enabled") $off.stderr
                assert ($off.stderr | str contains $t) $off.stderr
                let unknown = (do { ^nu $RESOLVER json no-such-workload --target $t } | complete)
                assert ($unknown.stderr | str contains "no workload named") $unknown.stderr
            }
        }

        "enabled is true or false and nothing else": {
            with_target { workloads: { kube-observer: { enabled: "no" } } } {|t|
                let r = (do { ^nu $RESOLVER check --target $t } | complete)
                assert ($r.exit_code != 0)
                assert (($r.stdout + $r.stderr) | str contains "enabled") ($r.stdout + $r.stderr)
            }
        }

        "a disabled workload that is still in the target's apply order is a problem, named": {
            with_target { workloads: { kube-observer: { enabled: false } } } {|t|
                let target_nu = ($INFRA | path join "targets" $t "target.nu")
                '{ apply_order: ["kube-observer-resources.yaml", "kube-observer.yaml", "kube-consumer.yaml"] } | to nuon' | save --force $target_nu
                let r = (do { ^nu $RESOLVER check --target $t } | complete)
                let out = ($r.stdout + $r.stderr)
                assert ($r.exit_code != 0) $out
                assert ($out | str contains "kube-observer.yaml") $out
                assert ($out | str contains "kube-observer-resources.yaml") $out
                assert (not ($out | str contains "kube-consumer.yaml")) "a workload that is enabled was named"
            }
        }

        "a target that disables a workload and leaves it out of its apply order checks clean": {
            # Starting from a real target's configuration: a bare one has no Jira address and fails for that alone.
            let real = (open ($INFRA | path join "targets/local-minimal/config.nuon"))
            with_target ($real | merge deep { workloads: { kube-observer: { enabled: false } } }) {|t|
                let target_nu = ($INFRA | path join "targets" $t "target.nu")
                '{ apply_order: ["kube-consumer.yaml"] } | to nuon' | save --force $target_nu
                let r = (do { ^nu $RESOLVER check --target $t } | complete)
                assert equal $r.exit_code 0 ($r.stdout + $r.stderr)
            }
        }

        "the health settings the agent validates are in what the agent is given": {
            let r = (resolve "kube-consumer")
            assert equal (env_value $r "POLAR_HEALTH_REJUVENATION_SECS") "300"
            assert equal (env_value $r "POLAR_HEALTH_STALE_SECS") "90"
            assert equal (env_value $r "POLAR_HEALTH_TICK_SECS") "30"
            assert equal (env_value $r "POLAR_HEALTH_EXPIRY_SECS") "60"
        }

        "the settings the rules read are available for each workload": {
            let st = (settings "kube-consumer")
            assert equal $st.shutdown.budget_secs 45
            assert equal $st.shutdown.grace_margin_secs 10
            assert equal $st.shutdown.termination_grace_secs 55
            assert equal $st.health.rejuvenation_secs 300
        }

        "the real catalog passes `check`, including the agent's own environment check": {
            let r = (^nu ($RESOLVER) check | complete)
            assert equal $r.exit_code 0 $r.stderr
            assert ($r.stdout | str contains "resolve cleanly")
        }

        "the credentials consumer keeps its ledger on a persistent volume it is given as a claim": {
            let r = (resolve "credentials-agent")
            let data = ($r.volumes | where name == "credentials-agent-data")
            assert equal ($data | length) 1
            assert equal ($data | first | get claim) "credentials-agent-data"
            assert equal ($r.mounts | where name == "credentials-agent-data" | first | get path) "/var/lib/credentials-agent"
            let res = (^nu $RESOLVER resources credentials-agent | from json)
            assert equal ($res.pvcs | get name) ["credentials-agent-data"]
        }

        "a workload whose code does not need the graph can still be given it": {
            # The consumer writes the graph through its own section and must not need it to be
            # healthy, so the entry provides it; the pod gets the same as for any graph-write.
            let r = (resolve "credentials-agent")
            assert ("graph-write" in $r.needs)
            assert (env_value $r "GRAPH_ENDPOINT" | is-not-empty)
            assert (env_value $r "POLAR_HEALTH_DEP_ENDPOINTS" | str contains "polar-db-svc")
            assert equal (env_value $r "POLAR_CAPABILITIES" | split row "," | sort) ($r.needs | sort)
        }

        "a workload that must start without its dependencies is watched for them but does not wait at start": {
            let consumer = (resolve "credentials-agent")
            assert equal $consumer.dep_checks []
            let health = (env_value $consumer "POLAR_HEALTH_DEP_ENDPOINTS")
            assert ($health | str contains "cassini-ip-svc")
            assert ($health | str contains "polar-db-svc")
            # and one that does not say so waits, as every agent did
            assert (resolve "kube-consumer" | get dep_checks | is-not-empty)
        }

        "the escrow authority's polar-init is told to skip the escrow bootstrap, and no other is": {
            let consumer = (resolve "credentials-agent")
            assert ($consumer.init_env | any {|e| $e.name == "POLAR_INIT_SKIP_ESCROW_BOOTSTRAP" and $e.value == "true" })
            let other = (resolve "kube-consumer")
            assert ($other.init_env | where name == "POLAR_INIT_SKIP_ESCROW_BOOTSTRAP" | is-empty)
        }

        "the observer is a Service on the port its code listens on": {
            let r = (resolve "credentials-agent-observer")
            assert equal $r.ports [8080]
            assert equal (env_value $r "CREDENTIALS_AGENT_OBSERVER_PORT") "8080"
            let res = (^nu $RESOLVER resources credentials-agent-observer | from json)
            assert equal ($res.services | get name) ["credentials-agent-observer"]
            assert equal ($res.services | first | get selector) "credentials-agent-observer"
            assert equal ($res.services | first | get ports | get port) [8080]
        }

        "both trust cert-issuer's CA from the first request, as a mounted file": {
            for n in ["credentials-agent" "credentials-agent-observer"] {
                let r = (resolve $n)
                assert ($r.volumes | any {|v| $v.name == "polar-ca" and $v.config_map == "polar-ca" }) $n
                assert ($r.init_env | any {|e| $e.name == "POLAR_INIT_CA_CERT" and $e.value == "/etc/polar-ca/ca.pem" }) $n
                assert ($r.init_mounts | any {|m| $m.path == "/etc/polar-ca" }) $n
            }
            assert equal (env_value (resolve "credentials-agent") "CREDENTIALS_AGENT_CA_CERT") "/etc/polar-ca/ca.pem"
        }

        "the consumer is told where cert-issuer is, from the catalog's facts": {
            let r = (resolve "credentials-agent")
            assert equal (env_value $r "CREDENTIALS_AGENT_CERT_ISSUER_URL") $r.cert_issuer_url
        }

        "what the built image says about itself is what the source tree says": {
            # The scaffold taken from the image in the local registry, the artifact that deploys,
            # must give the record the source tree gives. Skipped, and said so, where there is no
            # podman or the image has not been pushed.
            let have = (which podman | is-not-empty) and (try { (^curl -sf $"(($env.POLAR_REGISTRY? | default 'localhost:5000'))/v2/kube-consumer/tags/list" | from json | get tags | is-not-empty) } catch { false })
            if not $have {
                print "        (skipped: no podman, or kube-consumer is not in the local registry)"
            } else {
                for n in ["kube-consumer" "credentials-agent"] {
                    let from_source = (^nu $RESOLVER json $n | from json)
                    let from_image = (with-env { POLAR_AGENT_SOURCE: "images" } { ^nu $RESOLVER json $n | from json })
                    assert equal $from_image $from_source $n
                }
            }
        }

        "the broker is a service, not a client: it needs nothing and is not given a Cassini client": {
            let r = (resolve "cassini")
            assert ("cassini" not-in $r.needs)
            assert equal $r.kind "service"
            # none of the client's variables are in its document, and it does not wait for Cassini
            let maps = (^nu $RESOLVER resources cassini | from json | get config_maps)
            let sections = ($maps | first | get content | from json | get sections | get name)
            assert ("cassini-client" not-in $sections)
            assert ("cassini-broker" in $sections)
            assert equal $r.dep_checks []
        }

        "the broker is given a server certificate for the Cassini service's name, read-only": {
            let r = (resolve "cassini")
            assert equal $r.certs ["server:/etc/tls/certs:ecdsa-p256:cassini-ip-svc.polar.svc.cluster.local"]
            assert ($r.mounts | any {|m| $m.name == "polar-certs" and $m.read_only })
            assert equal (env_value $r "TLS_SERVER_CERT_CHAIN") "/etc/tls/certs/cert.pem"
            assert equal (env_value $r "CASSINI_BIND_ADDR") "0.0.0.0:8080"
        }

        "the broker's shutdown token is a secret the pod is given from a Secret": {
            let r = (resolve "cassini")
            let token = ($r.env | where name == "CASSINI_SHUTDOWN_TOKEN")
            assert equal ($token | length) 1
            assert equal ($token | first | get secret) "cassini-shutdown-token"
            assert equal ($token | first | get key) "token"
            assert equal ($token | first | get value) null
        }

        "the broker starts without the escrow bootstrap, which needs the broker": {
            let r = (resolve "cassini")
            assert ($r.init_env | any {|e| $e.name == "POLAR_INIT_SKIP_ESCROW_BOOTSTRAP" and $e.value == "true" })
        }

        "the broker's Service has the name its clients use, selects the broker, and exposes the one port it listens on": {
            let r = (resolve "cassini")
            assert equal $r.ports [8080]
            let res = (^nu $RESOLVER resources cassini | from json)
            let service = ($res.services | first)
            assert equal $service.name "cassini-ip-svc"
            assert equal $service.selector "cassini"
            assert equal ($service.ports | get port) [8080]
            assert equal ($service.ports | get name) ["cassini-tcp"]
        }

        "every workload exports its traces to Jaeger's OTLP port, not the UI's": {
            for n in ["cassini" "kube-consumer" "kube-observer" "jira-observer" "jira-processor" "credentials-agent" "credentials-agent-observer"] {
                let r = (resolve $n)
                assert equal (env_value $r "JAEGER_OTLP_ENDPOINT") "http://jaeger-svc.polar.svc.cluster.local:4318/v1/traces" $n
            }
        }

        "the broker's logging is the deployment's section every agent has, not a variable of its own": {
            let r = (resolve "cassini")
            assert equal (env_value $r "CASSINI_LOG") null
            assert equal (env_value $r "RUST_LOG") null "the default, info, is not written down"
        }

        "the broker's health check watches the CA's certificate as well as its own": {
            let r = (resolve "cassini")
            assert equal (env_value $r "POLAR_HEALTH_CERTS") "/etc/tls/certs/cert.pem,/etc/tls/certs/ca.pem"
            # an agent still watches its own certificate only
            let agent = (resolve "kube-consumer")
            assert equal (env_value $agent "POLAR_HEALTH_CERTS") "/etc/tls/certs/cert.pem"
        }

        "a kind can extend another and change only what differs": {
            let kinds = (open ($RESOLVER | path dirname | path join "config/kinds.nuon"))
            assert equal ($kinds.service.extends) "agent"
            # not a client, but it still sends its traces
            assert equal ($kinds.service | get -o needs) ["tracing"]
        }

        "the resolver reads its registry credentials from a Secret, mounted read-only where it is told to look": {
            let r = (resolve_for "local" "oci-registry-resolver")
            let volume = ($r.volumes | where name == "oci-registry-auth")
            assert equal ($volume | length) 1
            assert equal ($volume | first | get secret) "oci-registry-auth"
            assert equal ($volume | first | get items) [{ key: "oci-registry-auth", path: "config.json" }]
            assert equal ($r.mounts | where name == "oci-registry-auth" | first | get path) "/etc/oci-registry-auth"
            assert ($r.mounts | where name == "oci-registry-auth" | first | get read_only)
            assert equal (env_value $r "DOCKER_CONFIG" "local") "/etc/oci-registry-auth"
        }

        "the resolver speaks http to the cluster's own development registry, which the target names": {
            let r = (resolve_for "local" "oci-registry-resolver")
            let cluster = (open ($RESOLVER | path dirname | path join "config/cluster.nuon"))
            assert ((env_value $r "OCI_INSECURE_REGISTRIES" "local") | str contains $cluster.registry)
        }

        "the git observer is a plain agent: Cassini only, its cache in the pod, and no graph": {
            let r = (resolve "git-repo-observer")
            assert ("graph-write" not-in $r.needs)
            assert equal (env_value $r "POLAR_CACHE_ROOT") "/var/lib/git-repo-observer/cache"
            assert ($r.mounts | any {|m| $m.path == "/var/lib/git-repo-observer" and not $m.read_only })
            assert equal $r.service_account "git-repo-observer"
        }

        "the git processor writes to the graph and needs no cache; the controller does neither": {
            let r = (resolve "git-repo-processor")
            assert ("graph-write" in $r.needs)
            assert equal $r.service_account "git-repo-processor"
            let c = (resolve "git-repo-controller")
            assert ("graph-write" not-in $c.needs)
            assert equal $c.service_account "git-repo-controller"
            assert ($c.mounts | where path =~ "cache" | is-empty)
        }

        "a volume that is not a Secret with items has none": {
            let r = (resolve "kube-consumer")
            assert ($r.volumes | all {|v| $v.items == [] })
        }

        "the bootstrap identities ask cert-issuer, and every other workload does until a target moves it": {
            # An empty target, so this says what the catalog's defaults are and not how far the rollout has got.
            with_target { workloads: {} } {|t|
                for n in [cassini credentials-agent credentials-agent-observer] {
                    let r = (^nu $RESOLVER json $n --target $t | from json)
                    let st = (^nu $RESOLVER settings $n --target $t | from json)
                    assert equal $st.issuer "cert-issuer" $n
                    assert $st.bootstrap $n
                    assert ($r.cert_issuer_url | str starts-with "https://cert-issuer.") $n
                }
                let k = (^nu $RESOLVER json kube-consumer --target $t | from json)
                let ks = (^nu $RESOLVER settings kube-consumer --target $t | from json)
                assert equal $ks.issuer "cert-issuer"
                assert (not $ks.bootstrap)
                assert equal (init_env_value $k "POLAR_INIT_ISSUER") null
            }
        }

        "a workload moved onto the credentials agent is told the observer's address and not cert-issuer's": {
            with_target { workloads: { kube-consumer: { issuer: "credentials-agent" } } } {|t|
                let r = (^nu $RESOLVER json kube-consumer --target $t | from json)
                assert equal (^nu $RESOLVER settings kube-consumer --target $t | from json | get issuer) "credentials-agent"
                assert equal (init_env_value $r "POLAR_INIT_ISSUER") "credentials-agent"
                assert equal (init_env_value $r "POLAR_INIT_OBSERVER_URL") "http://credentials-agent-observer.polar.svc.cluster.local:8080"
                assert equal $r.cert_issuer_url ""
            }
        }

        "a bootstrap identity cannot be moved onto the credentials agent": {
            with_target { workloads: { cassini: { issuer: "credentials-agent" } } } {|t|
                let r = (^nu $RESOLVER check --target $t | complete)
                assert ($r.exit_code != 0)
                assert ($r.stderr | str contains "cassini")
            }
        }

        "an issuer that is neither is refused": {
            with_target { workloads: { kube-consumer: { issuer: "vault" } } } {|t|
                let r = (^nu $RESOLVER check --target $t | complete)
                assert ($r.exit_code != 0)
                assert ($r.stderr | str contains "vault")
            }
        }

        "workloads still asking cert-issuer are listed as pending while the floor is off, and refused once it is enforced": {
            # Everything on cert-issuer, as the catalog's defaults have it. The Jira observer needs the one
            # setting a target gives it, or `check` would fail for that and not for what this is about.
            let jira = { jira-observer: { env: [
                { name: "JIRA_URL", value: "https://example.atlassian.net" }
                { name: "JIRA_EMAIL", value: "someone@example.com" }
            ] } }
            with_target { workloads: $jira } {|t|
                let off = (^nu $RESOLVER check --target $t | complete)
                assert equal $off.exit_code 0 $off.stderr
                assert ($off.stdout | str contains "pending")
                assert ($off.stdout | str contains "kube-consumer")
            }
            with_target { cluster: { issuance: { floor_enforced: true } }, workloads: $jira } {|t|
                let on = (^nu $RESOLVER check --target $t | complete)
                assert ($on.exit_code != 0)
                assert ($on.stderr | str contains "kube-consumer")
            }
        }

        "on the local-minimal target no catalog workload asks cert-issuer directly but the bootstrap identities": {
            # The state phase 6 of the cert-issuer isolation reached (docs/architecture/cert-issuer-isolation.md).
            let r = (^nu $RESOLVER check | complete)
            assert equal $r.exit_code 0 $r.stderr
            assert (not ($r.stdout | str contains "pending"))
        }

        "the floor is written from the catalog: the bootstrap identities, then a vouched-only entry for each server certificate on the credentials agent": {
            let f = (^nu $RESOLVER floor | from json)
            let cassini = ($f | where workload == "system:serviceaccount:polar:cassini" | first)
            assert equal $cassini.certType "SERVER"
            assert equal $cassini.names ["cassini-ip-svc.polar.svc.cluster.local"]
            assert (not $cassini.vouchedOnly)
            for w in [credentials-agent credentials-agent-observer] {
                let e = ($f | where workload == $"system:serviceaccount:polar:($w)" | first)
                assert equal $e.certType "CLIENT"
            }
            # A workload still on cert-issuer has no entry; one moved onto the credentials agent that is issued a
            # server certificate has a vouched-only one.
            with_target { workloads: {} } {|t|
                let before = (^nu $RESOLVER floor --target $t | from json)
                assert equal ($before | where workload == "system:serviceaccount:polar:polar-git-server" | length) 0
            }
            with_target { workloads: { polar-git-server: { issuer: "credentials-agent" } } } {|t|
                let moved = (^nu $RESOLVER floor --target $t | from json)
                let g = ($moved | where workload == "system:serviceaccount:polar:polar-git-server" | first)
                assert equal $g.certType "SERVER"
                assert $g.vouchedOnly
                assert equal $g.names ["polar-git-server.polar.svc.cluster.local"]
            }
        }

        "scaffolds writes one file for each agent, named by the name the agent gives itself": {
            # Stand-in scaffolds, so nothing is built: one per workload, each naming its agent differently from
            # the workload, to show the file is named by what the agent says.
            let stubs = (mktemp --directory)
            let out = (mktemp --directory)
            let names = (^nu $RESOLVER names | from json)
            for n in $names {
                { agent: { name: $"agent-of-($n)" }, sections: [], runtime: [] } | to json | save --force ($stubs | path join $"($n).json")
            }
            let agents = ($names | where {|n| (resolve $n).kind == "agent" } | length)
            with-env { POLAR_SCAFFOLD_DIR: $stubs } { ^nu $RESOLVER scaffolds $out | ignore }
            let written = (ls $out | get name | each {|f| $f | path basename } | sort)
            assert equal ($written | length) $agents
            assert ($written | all {|f| $f | str starts-with "agent-of-" })
            let one = (open ($out | path join ($written | first)))
            assert ($one.agent.name | str starts-with "agent-of-")
            rm -rf $stubs $out
        }

        "a graph agent presents the pod's one client certificate to Neo4j and does not ask for a second": {
            # Two client certificates for one pod instance is what the credentials agent refuses (and a second
            # would revoke the first), and the second was the same identity and type with no extra names, so
            # Neo4j is shown the certificate the pod already has.
            for n in [kube-consumer jira-processor polar-scheduler git-repo-processor credentials-agent] {
                let r = (resolve $n)
                assert equal $r.certs ["client:/etc/tls/certs:ecdsa-p256:"] $n
                assert ($r.volumes | where name == "neo4j-client-tls" | is-empty) $n
                assert ($r.mounts | where name == "neo4j-client-tls" | is-empty) $n
                assert ($r.init_mounts | where name == "neo4j-client-tls" | is-empty) $n
                assert equal (env_value $r "GRAPH_CLIENT_CERT") "/etc/tls/certs/cert.pem" $n
                assert equal (env_value $r "GRAPH_CLIENT_KEY") "/etc/tls/certs/key.pem" $n
                assert equal (env_value $r "GRAPH_CA_CERT") "/etc/tls/certs/ca.pem" $n
            }
        }

        "the health check watches the one certificate a graph agent has": {
            let r = (resolve "kube-consumer")
            let certs = (env_value $r "POLAR_HEALTH_CERTS")
            assert ($certs | str contains "/etc/tls/certs/cert.pem")
            assert (not ($certs | str contains "neo4j-client-tls"))
        }

        "the address of a service comes from services.nuon, and a service with no URL is an error": {
            let url = (^nu $RESOLVER service-url credentials-agent-observer | str trim)
            assert equal $url "http://credentials-agent-observer.polar.svc.cluster.local:8080"
            let missing = (^nu $RESOLVER service-url cassini | complete)
            assert ($missing.exit_code != 0)
            let unknown = (^nu $RESOLVER service-url no-such-service | complete)
            assert ($unknown.exit_code != 0)
        }

        "the template does not see the settings record": {
            # render-catalog hands the template record to json-to-dhall, which rejects
            # a field its schema does not have.
            let r = (resolve "kube-consumer")
            assert ("_settings" not-in ($r | columns))
            assert ("termination_grace_secs" in ($r | columns))
        }
    }

    let results = ($tests | transpose name body | each {|t|
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
