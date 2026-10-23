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

# infra/tests/ca.nu
#
# cert-issuer as a catalog workload of its own kind, `ca` (docs/architecture/cert-issuer-isolation.md, step 1c): it is
# the issuer, so its pod has no polar-init; it is given its own token, the Kubernetes API token and its CA; its health
# does not wait on Cassini; its floor is written from every workload's spec and scaffold, its own entry among them,
# without resolving cert-issuer again; and the document it is given is one it accepts. No cluster: the scaffolds of
# cert-issuer and Cassini are built here.
#
#   nu infra/tests/ca.nu

use std/assert
use ../registry.nu [node_registry]

const INFRA = (path self | path dirname | path dirname)
const RESOLVER = (path self | path dirname | path dirname | path join "resolve.nu")
const AGENTS = (path self | path dirname | path dirname | path dirname | path join "src/agents")

# The issuers a target trusts, as its config gives them.
const ISSUERS = [{
    issuer: "https://kubernetes.default.svc.cluster.local"
    audience: ["polar-cert-issuer.local"]
    jwks_uri: "https://kubernetes.default.svc.cluster.local/openid/v1/jwks"
    workload_identity_claim: "sub"
    instance_binding_claim: "kubernetes.io/pod/uid"
    identity_normalization: { strategy: "kubernetes_service_account", cluster_domain: "cluster.local" }
    identity_scope_claim: null
    allowed_algorithms: ["RS256"]
    jwks_cache_ttl_min: { secs: 30, nanos: 0 }
    jwks_cache_ttl_max: { secs: 3600, nanos: 0 }
}]

# A throwaway target running cert-issuer and Cassini alone, with the floor enforced or not.
def with_ca_target [enforced: bool, body: closure] {
    let others = (^nu $RESOLVER names --target local | from json | where {|n| not ($n in ["cassini" "cert-issuer"]) })
    let workloads = ($others | reduce --fold {} {|n, acc| $acc | insert $n { enabled: false } }
        | insert cert-issuer { env: [ { name: "CERT_ISSUER_ISSUERS", value: $ISSUERS } ] })
    let name = $"zz-test-(random chars --length 8)"
    let dir = ($INFRA | path join "targets" $name)
    mkdir $dir
    { cluster: { issuance: { floor_enforced: $enforced } }, workloads: $workloads } | to nuon | save --force ($dir | path join "config.nuon")
    try {
        do $body $name
        rm -rf $dir
    } catch {|e|
        rm -rf $dir
        error make { msg: $e.msg }
    }
}

def resolved [target: string] { ^nu $RESOLVER json cert-issuer --target $target | from json }

# The variables cert-issuer's document gives it.
def document [target: string] {
    ^nu $RESOLVER resources cert-issuer --target $target | from json | get config_maps | first | get content | from json
}

def variable [doc: record, name: string] {
    $doc.sections | each {|s| $s.variables } | flatten | where name == $name | get -o 0.value
}

# The manifests the catalog renders for the target: cert-issuer's and Cassini's.
def rendered [target: string] {
    let out = (mktemp --directory)
    let r = (^nu ($INFRA | path join "render-catalog.nu") $target $out | complete)
    if $r.exit_code != 0 { error make { msg: $"rendering failed: ($r.stdout) ($r.stderr)" } }
    $out
}

def job_pod [out: string] { open --raw ($out | path join "cert-issuer.yaml") | from yaml | get spec.template.spec }

def main [] {
    $env.POLAR_NODE_REGISTRY = (node_registry local-minimal)
    # The scaffolds the resolver reads, from the binaries themselves.
    let scaffolds = (mktemp --directory)
    for agent in [[name package bin]; [cert-issuer cert-issuer cert-issuer] [cassini cassini-broker cassini-server]] {
        let r = (do { ^cargo run -q --manifest-path ($AGENTS | path join "Cargo.toml") -p $agent.package --bin $agent.bin -- --scaffold } | complete)
        if $r.exit_code != 0 { error make { msg: $"($agent.name) --scaffold failed: ($r.stderr)" } }
        $r.stdout | save --force ($scaffolds | path join $"($agent.name).json")
    }
    $env.POLAR_SCAFFOLD_DIR = $scaffolds
    let cert_issuer = ($AGENTS | path join "target/debug/cert-issuer")

    let tests = {
        "cert-issuer is a catalog workload of kind ca": {
            with_ca_target true {|t|
                let r = (resolved $t)
                assert equal $r.kind "ca"
                assert ("cert-issuer" in (^nu $RESOLVER names --target $t | from json))
            }
        }

        "its pod has no polar-init, and its own token, the API token and its CA": {
            with_ca_target true {|t|
                let pod = (job_pod (rendered $t))
                assert ($pod | get -o initContainers | default [] | is-empty) "no init container: it is the issuer"
                let token = ($pod.volumes | where name == "sa-token" | get 0.projected.sources.0.serviceAccountToken)
                assert equal $token.audience "polar-cert-issuer.local"
                let container = ($pod.containers.0)
                assert (($container.volumeMounts | where name == "sa-token" and mountPath == "/workspace") | is-not-empty) "its own token, in its own container"
                assert equal $pod.automountServiceAccountToken true
                let ca = ($pod.volumes | where name == "cert-issuer-ca" | get 0.secret)
                assert equal $ca.secretName "cert-issuer-ca"
                assert equal $ca.defaultMode 288
                assert equal ($ca.items | get path | sort) ["ca/ca.crt" "ca/ca.key"]
                assert equal $pod.securityContext.fsGroup 1000
                assert equal ($container.livenessProbe.exec.command) ["polar-healthcheck"]
                let data = ($container.volumeMounts | where mountPath == "/var/lib/cert-issuer" | get 0)
                assert equal ($pod.volumes | where name == $data.name | get 0.persistentVolumeClaim.claimName) "cert-issuer-data"
                assert (($data | get -o readOnly) != true) "its record is written"
            }
        }

        "its document is complete and its health does not wait on Cassini": {
            with_ca_target true {|t|
                let doc = (document $t)
                assert equal (variable $doc "CERT_ISSUER_OWN_TOKEN") "/workspace/token"
                assert equal (variable $doc "CERT_ISSUER_SERVER_IDENTITY") "cert-issuer.polar.svc.cluster.local"
                assert equal (variable $doc "CERT_ISSUER_CA_PROVISIONING") "provisioned"
                assert equal (variable $doc "CERT_ISSUER_ISSUERS" | from json | get 0.issuer) "https://kubernetes.default.svc.cluster.local"
                assert equal (variable $doc "TLS_CA_CERT") "/etc/tls/certs/ca.pem"
                # Its record, on persistent storage.
                assert equal (variable $doc "CERT_ISSUER_RECORD_PATH") "/var/lib/cert-issuer/issued.jsonl"
                let res = (^nu $RESOLVER resources cert-issuer --target $t | from json)
                assert equal ($res.pvcs | get name) ["cert-issuer-data"]
                assert ((variable $doc "POLAR_HEALTH_DEP_ENDPOINTS" | default "") !~ "cassini") "Cassini is not a health dependency"
            }
        }

        "its floor holds its own entry from its scaffold, written only when enforced": {
            with_ca_target true {|t|
                let floor = (variable (document $t) "CERT_ISSUER_BOOTSTRAP" | from json)
                let own = ($floor | where workload == "system:serviceaccount:polar:cert-issuer")
                assert equal ($own | length) 1
                assert equal $own.0.cert_type "CLIENT"
                assert equal $own.0.names []
                assert equal $own.0.groups ["cassini-client"]
                assert (($floor | where workload == "system:serviceaccount:polar:cassini") | is-not-empty) "every bootstrap service's"
            }
            with_ca_target false {|t|
                assert equal (variable (document $t) "CERT_ISSUER_BOOTSTRAP") null "unset is the open floor"
            }
        }

        "the floor carries the entries rendered outside the catalog": {
            with_ca_target true {|t|
                let neo4j = { workload: "system:serviceaccount:polar-graph:graph", cert_type: "SERVER", names: ["neo4j"], vouched_only: true, publish: [], subscribe: [], groups: [] }
                with-env { POLAR_FLOOR_EXTRA: ([$neo4j] | to json --raw) } {
                    let floor = (variable (document $t) "CERT_ISSUER_BOOTSTRAP" | from json)
                    assert equal ($floor | where workload == $neo4j.workload | first) $neo4j
                }
            }
        }

        "the floor is built without resolving cert-issuer from inside its own resolve": {
            with_ca_target true {|t|
                let floor = (^nu $RESOLVER floor --target $t | from json)
                assert (($floor | where workload == "system:serviceaccount:polar:cert-issuer") | is-not-empty)
            }
        }

        "the document is one cert-issuer itself accepts": {
            with_ca_target true {|t|
                let file = (mktemp --suffix ".json")
                document $t | to json | save --force $file
                let r = (do { ^$cert_issuer --check --config $file } | complete)
                assert equal $r.exit_code 0 $"($r.stdout)($r.stderr)"
            }
        }

        "another catalog workload renders with the agent template as before": {
            with_ca_target true {|t|
                let pod = (open --raw ((rendered $t) | path join "cassini.yaml") | from yaml | get spec.template.spec)
                assert (($pod.initContainers | where name == "polar-init") | is-not-empty)
                assert (($pod | get -o securityContext) == null)
            }
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
        if $r.error == null { print $"  ok    ($r.name)" } else { print -e $"  FAIL  ($r.name): ($r.error)" }
    }
    let failed = ($results | where error != null | length)
    if $failed > 0 {
        print -e $"($failed) of ($results | length) failed"
        exit 1
    }
    print $"all ($results | length) passed"
}
