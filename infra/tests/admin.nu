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

# infra/tests/admin.nu
#
# What up.nu does with ~/.config/polar-admin that belongs to another cluster: the CA stored there is not this
# cluster's, so nothing in it can be used here, and it is moved aside under a name that cannot overwrite
# an earlier one. Nothing is deleted.
#
#   nu infra/tests/admin.nu

use std/assert
use ../admin.nu *

def main [] {
    let tests = [
        {
            name: "a directory is moved aside to a name with the time, beside it"
            body: {
                assert equal (aside_path "/home/u/.config/polar-admin" "20261014-135100" []) "/home/u/.config/polar-admin.old-20261014-135100"
            }
        }
        {
            name: "a name already taken is not reused, so an earlier one is never overwritten"
            body: {
                let taken = ["/home/u/.config/polar-admin.old-20261014-135100"]
                assert equal (aside_path "/home/u/.config/polar-admin" "20261014-135100" $taken) "/home/u/.config/polar-admin.old-20261014-135100-2"
                let more = ($taken ++ ["/home/u/.config/polar-admin.old-20261014-135100-2"])
                assert equal (aside_path "/home/u/.config/polar-admin" "20261014-135100" $more) "/home/u/.config/polar-admin.old-20261014-135100-3"
            }
        }
        {
            name: "material is another cluster's when the CA stored with it is not this cluster's, and only then"
            body: {
                assert (is_another_clusters "-----BEGIN CERTIFICATE-----\nAAA\n-----END CERTIFICATE-----\n" "-----BEGIN CERTIFICATE-----\nBBB\n-----END CERTIFICATE-----")
                # the same CA, whatever the trailing whitespace, is this cluster's
                assert (not (is_another_clusters "-----BEGIN CERTIFICATE-----\nAAA\n-----END CERTIFICATE-----\n" "-----BEGIN CERTIFICATE-----\nAAA\n-----END CERTIFICATE-----"))
                # nothing stored says nothing: there is no stale material to move
                assert (not (is_another_clusters null "-----BEGIN CERTIFICATE-----\nAAA\n-----END CERTIFICATE-----"))
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
