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

# infra/tests/cassini-shutdown.nu
#
# Tests of what infra/cassini-shutdown.nu hands the shutdown tool: where it finds the broker, the
# name it verifies the broker against (from the catalog's facts, not typed again), and whose
# certificate it presents.
#
#   nu infra/tests/cassini-shutdown.nu

use std/assert
use ../cassini-shutdown.nu *

def main [] {
    let tests = [
        {
            name: "the broker's name is built from the catalog's service, namespace and cluster domain"
            body: {
                let services = { cassini: { dns_name: "cassini-ip-svc", namespace: "polar", port: 8080 } }
                let cluster = { domain: "cluster.local", namespaces: { polar: "polar" } }
                assert equal (cassini_server_name $services $cluster) "cassini-ip-svc.polar.svc.cluster.local"
                let moved = { domain: "example.test", namespaces: { polar: "ns-polar" } }
                assert equal (cassini_server_name $services $moved) "cassini-ip-svc.ns-polar.svc.example.test"
            }
        }
        {
            name: "the tool presents the user's certificate, finds the broker on the local forward, and carries the token"
            body: {
                let e = (shutdown_env "t0ken" "/home/u/.config/polar-admin" "alice" 18080 "cassini-ip-svc.polar.svc.cluster.local")
                assert equal $e.TLS_CLIENT_CERT "/home/u/.config/polar-admin/alice.crt"
                assert equal $e.TLS_CLIENT_KEY "/home/u/.config/polar-admin/alice.key"
                assert equal $e.TLS_CA_CERT "/home/u/.config/polar-admin/ca.pem"
                assert equal $e.BROKER_ADDR "127.0.0.1:18080"
                assert equal $e.CASSINI_SERVER_NAME "cassini-ip-svc.polar.svc.cluster.local"
                assert equal $e.CASSINI_SHUTDOWN_TOKEN "t0ken"
            }
        }
    ]

    let results = ($tests | each {|t|
        try { do $t.body; { name: $t.name, error: null } } catch {|e| { name: $t.name, error: $e.msg } }
    })
    for r in $results {
        if $r.error == null { print $"  ok    ($r.name)" } else { print -e $"  FAIL  ($r.name): ($r.error)" }
    }
    let failed = ($results | where error != null | length)
    if $failed > 0 { print -e $"($failed) of ($results | length) failed"; exit 1 }
    print $"all ($results | length) passed"
}
