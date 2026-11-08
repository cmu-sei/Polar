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

# What up.nu's first step asks of the cluster and of this machine, found out there and not steps later: Kubernetes 1.36
# or later (step 7 was the first to fail on 1.35), and an openssl with `x509 -ext` (step 11 was, on macOS's LibreSSL).

use std/assert
use ../preflight.nu *

def main [] {
    let tests = [
        {
            name: "Kubernetes 1.36 and later are enough, whatever a provider adds to the version"
            body: {
                for v in ["v1.36.0" "v1.37.1" "v1.37.1-gke.1200" "v1.40.0+k3s1" "v2.0.0"] {
                    assert equal (kubernetes_problem $v) null $v
                }
            }
        }
        {
            name: "an older Kubernetes is refused, naming its version, the one needed and what to do on minikube"
            body: {
                for v in ["v1.35.4" "v1.34.0" "v1.9.0" "v1.31.12-eks.1"] {
                    let p = (kubernetes_problem $v)
                    assert ($p != null) $v
                    assert ($p | str contains ($v | str replace "v" "" | split row "." | first 2 | str join ".")) $p
                    assert ($p | str contains "1.36 or later") $p
                    assert ($p | str contains "minikube") $p
                }
            }
        }
        {
            name: "a version that cannot be read is said so, not taken as new enough"
            body: {
                for v in ["" "unknown" "1.x"] {
                    let p = (kubernetes_problem $v)
                    assert ($p != null) $"'($v)'"
                }
            }
        }
        {
            name: "OpenSSL 1.1.1 and later have x509 -ext"
            body: {
                for v in ["OpenSSL 3.5.8 25 Aug 2026 (Library: OpenSSL 3.5.8 25 Aug 2026)" "OpenSSL 3.0.2 15 Mar 2022" "OpenSSL 1.1.1w  11 Sep 2023"] {
                    assert equal (openssl_problem $v) null $v
                }
            }
        }
        {
            name: "LibreSSL (macOS's openssl), an older OpenSSL, or none is refused, with the way to get OpenSSL 3"
            body: {
                for v in ["LibreSSL 3.3.6" "OpenSSL 1.1.0l  10 Sep 2019" "OpenSSL 1.0.2k-fips  26 Jan 2017" ""] {
                    let p = (openssl_problem $v)
                    assert ($p != null) $"'($v)'"
                    assert ($p | str contains "brew install openssl@3") $p
                }
                assert ((openssl_problem "LibreSSL 3.3.6") | str contains "LibreSSL 3.3.6")
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
