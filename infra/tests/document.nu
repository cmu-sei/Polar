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

# infra/tests/document.nu
#
# Tests of how the resolver fills an agent's scaffold (the configuration document the agent
# itself prints with --scaffold) from what the catalog gives a workload.
#
#   nu infra/tests/document.nu

use std/assert
use ../document.nu *

def var [name: string, extra: record = {}] {
    { name: $name, type: "text", required: false, secret: false, default: null, mutability: "static", doc: $"the ($name)", value: null } | merge $extra
}

def scaffold [] {
    {
        agent: { name: "demo", purpose: "a demo" }
        requires: ["cassini"]
        shutdown_budget_minimum_secs: 30
        format: 1
        sections: [
            { name: "cassini-client", capability: "cassini", variables: [
                (var "BROKER_ADDR" { required: true })
                (var "TLS_CLIENT_KEY" { required: true })
                (var "CASSINI_QUEUE_PATH")
            ] }
            { name: "graph", capability: "graph-write", variables: [
                (var "GRAPH_PASSWORD" { required: true, secret: true })
                (var "GRAPH_TICK" { default: "30" })
            ] }
        ]
    }
}

def literal [name: string, value: string, from: string = "cassini"] {
    { name: $name, value: $value, secret: null, key: null, from: $from }
}

def from_secret [name: string, secret: string, key: string] {
    { name: $name, value: null, secret: $secret, key: $key, from: "graph-write" }
}

def value_of [filled: record, name: string] {
    $filled.document.sections | each {|s| $s.variables } | flatten | where name == $name | first | get value
}

def main [] {
    let tests = [
        {
            name: "a value from the catalog is written into the variable it names"
            body: {
                let filled = (fill_document (scaffold) [(literal "BROKER_ADDR" "cassini:8080") (literal "TLS_CLIENT_KEY" "/k") (from_secret "GRAPH_PASSWORD" "pw" "secret")] "demo")
                assert equal (value_of $filled "BROKER_ADDR") "cassini:8080"
                assert equal $filled.problems []
            }
        }
        {
            name: "a secret is a reference to an environment variable of the same name, and the pod is told where it comes from"
            body: {
                let filled = (fill_document (scaffold) [(literal "BROKER_ADDR" "b") (literal "TLS_CLIENT_KEY" "/k") (from_secret "GRAPH_PASSWORD" "polar-graph-pw" "secret")] "demo")
                assert equal (value_of $filled "GRAPH_PASSWORD") { env: "GRAPH_PASSWORD" }
                assert equal $filled.secret_env [{ name: "GRAPH_PASSWORD", value: null, secret: "polar-graph-pw", key: "secret", from: "graph-write" }]
            }
        }
        {
            name: "a secret given as a literal is a problem that does not repeat it"
            body: {
                let filled = (fill_document (scaffold) [(literal "BROKER_ADDR" "b") (literal "TLS_CLIENT_KEY" "/k") (literal "GRAPH_PASSWORD" "hunter2")] "demo")
                assert equal ($filled.problems | length) 1
                assert ($filled.problems | first | str contains "GRAPH_PASSWORD")
                assert (not ($filled.problems | first | str contains "hunter2"))
            }
        }
        {
            name: "a value for a variable the agent does not declare is a problem naming where it came from"
            body: {
                let filled = (fill_document (scaffold) [(literal "BROKER_ADDR" "b") (literal "TLS_CLIENT_KEY" "/k") (from_secret "GRAPH_PASSWORD" "p" "k") (literal "MYSTERY" "x" "entry")] "demo")
                assert equal ($filled.problems | length) 1
                let p = ($filled.problems | first)
                assert ($p | str contains "MYSTERY")
                assert ($p | str contains "entry")
                assert ($p | str contains "demo")
            }
        }
        {
            name: "a required variable nothing gives a value is a problem saying what it is for"
            body: {
                let filled = (fill_document (scaffold) [(literal "BROKER_ADDR" "b") (from_secret "GRAPH_PASSWORD" "p" "k")] "demo")
                assert equal ($filled.problems | length) 1
                let p = ($filled.problems | first)
                assert ($p | str contains "TLS_CLIENT_KEY")
                assert ($p | str contains "the TLS_CLIENT_KEY")
            }
        }
        {
            name: "a variable with a default, or an optional one, need not be given"
            body: {
                let filled = (fill_document (scaffold) [(literal "BROKER_ADDR" "b") (literal "TLS_CLIENT_KEY" "/k") (from_secret "GRAPH_PASSWORD" "p" "k")] "demo")
                assert equal $filled.problems []
                assert equal (value_of $filled "GRAPH_TICK") null
                assert equal (value_of $filled "CASSINI_QUEUE_PATH") null
            }
        }
        {
            name: "every problem is reported, not the first"
            body: {
                let filled = (fill_document (scaffold) [(literal "NOPE" "x" "entry")] "demo")
                assert equal ($filled.problems | length) 4
            }
        }
        {
            name: "the pod's environment is the pointer to the document and the secrets it refers to, nothing else"
            body: {
                let filled = (fill_document (scaffold) [(literal "BROKER_ADDR" "b") (literal "TLS_CLIENT_KEY" "/k") (from_secret "GRAPH_PASSWORD" "polar-graph-pw" "secret")] "demo")
                let pod = (pod_env $filled "/etc/polar/config.json")
                assert equal ($pod | get name) ["POLAR_CONFIG" "GRAPH_PASSWORD"]
                assert equal ($pod | first | get value) "/etc/polar/config.json"
            }
        }
        {
            name: "the agent's own requirements and minimum budget are read from the scaffold"
            body: {
                assert equal (requires_of (scaffold)) ["cassini"]
                assert equal (budget_minimum_of (scaffold)) 30
            }
        }
        {
            name: "the document that goes into the ConfigMap is the scaffold, filled, as JSON the agent reads"
            body: {
                let filled = (fill_document (scaffold) [(literal "BROKER_ADDR" "b") (literal "TLS_CLIENT_KEY" "/k") (from_secret "GRAPH_PASSWORD" "p" "k")] "demo")
                let text = (document_json $filled)
                let back = ($text | from json)
                assert equal $back.format 1
                assert equal ($back.sections.0.variables.0.value) "b"
            }
        }
        {
            name: "the command an image runs is its entrypoint then its cmd, which is how the agent is asked to describe itself"
            body: {
                assert equal (image_command { Cmd: ["/bin/kube-consumer"] }) ["/bin/kube-consumer"]
                assert equal (image_command { Entrypoint: ["/bin/polar"], Cmd: ["agent"] }) ["/bin/polar" "agent"]
                assert equal (image_command { Entrypoint: ["/bin/agent"], Cmd: null }) ["/bin/agent"]
            }
        }
        {
            name: "an image that runs nothing is an error that names the problem"
            body: {
                let failed = (try { image_command { Env: [] }; null } catch {|e| $e.msg })
                assert ($failed != null)
                assert ($failed | str contains "entrypoint")
            }
        }
    ]

    let results = ($tests | each {|t|
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
