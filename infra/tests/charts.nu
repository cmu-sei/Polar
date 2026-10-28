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

# infra/tests/charts.nu
#
# What the hand-written service charts tell their pods: Neo4j's server certificate and the ACE job's
# client certificate come from the credentials agent, not from cert-issuer. Neither is a bootstrap identity,
# so cert-issuer will not sign either on its token alone once the floor is enforced
# (docs/architecture/cert-issuer-isolation.md). And cert-issuer's own chart gives it what the agent builder
# needs: its declared settings, its own token, the probe. Rendered the way render.nu renders them.
#
#   nu infra/tests/charts.nu

use std/assert
use ../registry.nu [node_registry]

const INFRA = (path self | path dirname | path dirname)
const OBSERVER = "http://credentials-agent-observer.polar.svc.cluster.local:8080"
# Not the default, so a chart that has it from anywhere but its context is found out.
const CREDENTIALS_AGENT = "credentials-agent.polar-test.serviceaccount.cluster.local"

def render [chart: string, extra: record] {
    let out = (mktemp --directory)
    let context = ({
        chart_dir: ($INFRA | path join "layers/2-services" $chart)
        output_dir: $out
        overrides: ($INFRA | path join "targets/local-minimal/overrides.dhall")
        target_dir: ($INFRA | path join "targets/local-minimal")
        enable_tls: true
        polarInitImage: "polar-init:test"
        neo4jBoltAddr: "polar-db-svc.polar-graph.svc.cluster.local:7687"
        certIssuerUrl: "https://cert-issuer.polar.svc.cluster.local:8443"
        certIssuerAudience: "polar-cert-issuer-neo4j.local"
        credentialsObserverUrl: $OBSERVER
        credentialsAgentIdentity: $CREDENTIALS_AGENT
    } | merge $extra)
    let r = (^nu ($INFRA | path join "layers/2-services" $chart "main.nu") ($context | to nuon) | complete)
    if $r.exit_code != 0 { error make { msg: $"rendering ($chart) failed: ($r.stderr)" } }
    $out
}

# The env of the polar-init container in the first document that has one.
def init_env [file: string] {
    open --raw $file
    | split row "\n---\n"
    | each {|d| $d | from yaml }
    | each {|doc|
        let pod = ($doc | get -o spec.template.spec)
        if $pod == null { null } else {
            $pod | get -o initContainers | default [] | where name == "polar-init" | get -o 0.env
        }
    }
    | compact
    | first
}

def value_of [vars: list, name: string] { $vars | where name == $name | get -o 0.value }

# The pod spec of the first document in a rendered file that has one.
def pod_of [file: string] {
    open --raw $file | split row "\n---\n" | each {|d| $d | from yaml | get -o spec.template.spec } | compact | first
}

# Whether polar-init in a rendered pod trusts cert-issuer's CA as the mounted polar-ca file, and the pod mounts it.
def pins_the_ca [file: string] {
    let pod = (pod_of $file)
    let init = ($pod.initContainers | where name == "polar-init" | first)
    assert equal (value_of $init.env "POLAR_INIT_CA_CERT") "/etc/polar-ca/ca.pem"
    let mount = ($init.volumeMounts | where mountPath == "/etc/polar-ca" | first)
    assert equal ($pod.volumes | where name == $mount.name | first | get configMap.name) "polar-ca"
}

# cert-issuer's chart, as render.nu renders it: the floor from the catalog (none here), whether it is enforced.
def render_cert_issuer [enforced: bool] {
    render cert-issuer { floor: [], floorEnforced: $enforced, identityLifetimeOverrides: [] }
}

# The pod spec of cert-issuer's Deployment.
def cert_issuer_pod [out: string] {
    open --raw ($out | path join "cert-issuer-deployment.yaml") | from yaml | get spec.template.spec
}

def cert_issuer_settings [out: string] {
    open --raw ($out | path join "cert-issuer-configmap.yaml") | from yaml | get data
}

def main [] {
    # The charts' Dhall reads the node registry from the environment, which render.nu sets; these render a chart without it.
    $env.POLAR_NODE_REGISTRY = (node_registry local-minimal)
    let tests = {
        # docs/architecture/deployments.md, step 3: the move to Deployments restarts a pod in place by a container's
        # restart rule, which the schema's Kubernetes types have from dhall-kubernetes 1.35.
        "the schema's containers take a restart rule, for agents that restart in place": {
            let file = (mktemp --suffix ".dhall")
            ([
                $"let k = ($INFRA)/schema/kubernetes.dhall"
                "in  k.Container::{"
                "    , name = \"agent\""
                "    , restartPolicyRules = Some"
                "      [ { action = \"RestartAllContainers\""
                "        , exitCodes = None { operator : Text, values : Optional (List Natural) }"
                "        }"
                "      ]"
                "    }"
            ] | str join "\n") | save --force $file
            let r = (do { ^dhall-to-yaml --file $file } | complete)
            rm $file
            assert equal $r.exit_code 0 $r.stderr
            assert equal ($r.stdout | from yaml | get restartPolicyRules) [{ action: "RestartAllContainers" }]
        }

        "Neo4j's server certificate is requested from the credentials agent": {
            let out = (render neo4j {})
            let vars = (init_env ($out | path join "neo4j-statefulset.yaml"))
            assert equal (value_of $vars "POLAR_INIT_ISSUER") "credentials-agent"
            assert equal (value_of $vars "POLAR_INIT_OBSERVER_URL") $OBSERVER
            # whose signature on a session offer it believes, as render.nu has it from the catalog
            assert equal (value_of $vars "POLAR_INIT_CREDENTIALS_AGENT_IDENTITY") $CREDENTIALS_AGENT
            # nothing in the pod is told where cert-issuer is
            assert (((value_of $vars "POLAR_INIT_CERT_ISSUER_URL") | default "") == "")
        }

        # docs/architecture/placement.md: Neo4j is the writers' (the maintainer, 2026-10-28), placed only on a target that
        # declares the pool, with both the selector and the toleration; a target that lacks the pool refuses it.
        "Neo4j goes to the writers' node where the target declares the pool, and nowhere in particular otherwise": {
            let placed = (render neo4j { placement: { pools: ["core" "observers" "writers"] } })
            let pod = (open ($placed | path join "neo4j-statefulset.yaml") | get spec.template.spec)
            assert equal $pod.nodeSelector { "polar.dev/pool": "writers" }
            assert equal $pod.tolerations [{ key: "polar.dev/pool", operator: "Equal", value: "writers", effect: "NoSchedule" }]
            for extra in [{} { placement: null }] {
                let plain = (open ((render neo4j $extra) | path join "neo4j-statefulset.yaml") | get spec.template.spec)
                assert equal ($plain.nodeSelector? | default null) null
                assert equal ($plain.tolerations? | default null) null
            }
            let refused = (try { render neo4j { placement: { pools: ["core"] } }; null } catch {|e| $e.msg })
            assert ($refused != null) "a target without the writers' pool"
            assert ($refused | str contains "writers") $refused
        }

        "Neo4j still asks for the server certificate it did and runs no escrow bootstrap": {
            let out = (render neo4j {})
            let text = (open --raw ($out | path join "neo4j-statefulset.yaml"))
            assert ($text | str contains "server:/etc/neo4j-tls:ecdsa-p256:")
            let vars = (init_env ($out | path join "neo4j-statefulset.yaml"))
            assert (($vars | where name == "POLAR_INIT_ESCROW_BOOTSTRAP") | is-empty)
            assert (($vars | where name == "POLAR_INIT_SKIP_ESCROW_BOOTSTRAP") | is-empty)
        }

        "Neo4j's configuration reaches its ConfigMap exactly, whatever characters it has": {
            # It is written into a Dhall string on the way, where a quote ends the string, a backslash starts an
            # escape and ${ starts an interpolation.
            let dir = (mktemp --directory)
            mkdir ($dir | path join "conf")
            let conf = "# a \"quoted\" word, a back\\slash, a ${brace} and a $dollar\nserver.x=1\n"
            $conf | save --force ($dir | path join "conf/neo4j-ssl.conf")
            let out = (render neo4j { target_dir: $dir })
            let map = (open --raw ($out | path join "neo4j-configmap.yaml") | from yaml)
            assert equal ($map | get data | get "neo4j.conf") $conf
        }

        "cert-issuer's settings reach it one declared variable each, and nothing names the file it read before": {
            let out = (render_cert_issuer true)
            let keys = (cert_issuer_settings $out | columns | sort)
            assert equal $keys ([
                CERT_ISSUER_BIND_ADDR CERT_ISSUER_BOOTSTRAP CERT_ISSUER_CA_CERT_PATH CERT_ISSUER_CA_KEY_PATH
                CERT_ISSUER_CA_PROVISIONING CERT_ISSUER_DEFAULT_LIFETIME_SECS CERT_ISSUER_IDENTITY_LIFETIME_OVERRIDES
                CERT_ISSUER_INTERNAL_CALLERS CERT_ISSUER_ISSUERS CERT_ISSUER_SERVER_IDENTITY
                CERT_ISSUER_SERVER_LIFETIME_SECS CERT_ISSUER_SERVICE_LIFETIME_SECS CERT_ISSUER_USER_LIFETIME_SECS
            ] | sort)
            # An agent's escrow certificate outlives its pods: a year, as an account's certificate.
            assert equal (cert_issuer_settings $out | get CERT_ISSUER_SERVICE_LIFETIME_SECS) "31536000"
            for key in [CERT_ISSUER_ISSUERS CERT_ISSUER_BOOTSTRAP CERT_ISSUER_IDENTITY_LIFETIME_OVERRIDES] {
                cert_issuer_settings $out | get $key | from json
            }
            let container = (cert_issuer_pod $out | get containers.0)
            assert equal ($container | get envFrom.0.configMapRef.name) "cert-issuer-config"
            assert (($container.env | where name == "CERT_ISSUER_CONFIG") | is-empty)
            assert (not (open --raw ($out | path join "cert-issuer-deployment.yaml") | str contains "/etc/cert-issuer"))
        }

        "cert-issuer's floor holds its own entry when it is enforced, and is unset, the open floor, when it is not": {
            let floor = (cert_issuer_settings (render_cert_issuer true) | get CERT_ISSUER_BOOTSTRAP | from json)
            let own = ($floor | where workload == "system:serviceaccount:polar:cert-issuer")
            assert equal ($own | length) 1
            assert equal ($own.0.cert_type) "CLIENT"
            assert equal ($own.0.groups) ["cassini-client"]
            assert equal ($own.0.names) []
            assert (not ((cert_issuer_settings (render_cert_issuer false)) | columns | any {|c| $c == "CERT_ISSUER_BOOTSTRAP" }))
        }

        "cert-issuer's pod has its own token, for its audience, and the probe and health every builder agent has": {
            let pod = (cert_issuer_pod (render_cert_issuer true))
            let token = ($pod.volumes | where name == "sa-token" | get 0.projected.sources.0.serviceAccountToken)
            assert equal $token.audience "polar-cert-issuer-neo4j.local"
            let container = ($pod.containers.0)
            assert equal (value_of $container.env "CERT_ISSUER_OWN_TOKEN") "/workspace/token"
            assert (($container.volumeMounts | where name == "sa-token" and mountPath == "/workspace") | is-not-empty)
            assert equal ($container.livenessProbe.exec.command) ["polar-healthcheck"]
            assert equal (value_of $container.env "POLAR_HEALTH_CERTS") (value_of $container.env "TLS_CLIENT_CERT")
            # cert-issuer writes the CA it signs under beside its own certificate, as the cassini capability names it,
            # not over the CA's own file, which is on a read-only Secret.
            assert equal (value_of $container.env "TLS_CA_CERT") "/etc/tls/certs/ca.pem"
            let tls = ($container.volumeMounts | where mountPath == "/etc/tls/certs" | get 0.name)
            assert (($pod.volumes | where name == $tls | get 0 | get -o emptyDir) != null)
            assert equal (value_of $container.env "POLAR_CAPABILITIES") "cassini"
        }

        "Neo4j's and the ACE job's polar-init trust cert-issuer's CA as a mounted file": {
            pins_the_ca ((render neo4j {}) | path join "neo4j-statefulset.yaml")
            pins_the_ca ((render ace {}) | path join "ace.yaml")
        }

        "the ACE job's client certificate is requested from the credentials agent": {
            let out = (render ace {})
            let vars = (init_env ($out | path join "ace.yaml"))
            assert equal (value_of $vars "POLAR_INIT_ISSUER") "credentials-agent"
            assert equal (value_of $vars "POLAR_INIT_OBSERVER_URL") $OBSERVER
            assert equal (value_of $vars "POLAR_INIT_CREDENTIALS_AGENT_IDENTITY") $CREDENTIALS_AGENT
            assert (((value_of $vars "POLAR_INIT_CERT_ISSUER_URL") | default "") == "")
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
