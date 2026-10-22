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
# What the hand-written service charts tell `polar-init`: Neo4j's server certificate and the ACE job's
# client certificate come from the credentials agent, not from cert-issuer. Neither is a bootstrap identity,
# so cert-issuer will not sign either on its token alone once the floor is enforced
# (docs/architecture/cert-issuer-isolation.md). Rendered the way render.nu renders them.
#
#   nu infra/tests/charts.nu

use std/assert
use ../registry.nu [node_registry]

const INFRA = (path self | path dirname | path dirname)
const OBSERVER = "http://credentials-agent-observer.polar.svc.cluster.local:8080"

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

def main [] {
    # The charts' Dhall reads the node registry from the environment, which render.nu sets; these render a chart without it.
    $env.POLAR_NODE_REGISTRY = (node_registry local-minimal)
    let tests = {
        "Neo4j's server certificate is requested from the credentials agent": {
            let out = (render neo4j {})
            let vars = (init_env ($out | path join "neo4j-statefulset.yaml"))
            assert equal (value_of $vars "POLAR_INIT_ISSUER") "credentials-agent"
            assert equal (value_of $vars "POLAR_INIT_OBSERVER_URL") $OBSERVER
            # nothing in the pod is told where cert-issuer is
            assert (((value_of $vars "POLAR_INIT_CERT_ISSUER_URL") | default "") == "")
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

        "the ACE job's client certificate is requested from the credentials agent": {
            let out = (render ace {})
            let vars = (init_env ($out | path join "ace.yaml"))
            assert equal (value_of $vars "POLAR_INIT_ISSUER") "credentials-agent"
            assert equal (value_of $vars "POLAR_INIT_OBSERVER_URL") $OBSERVER
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
