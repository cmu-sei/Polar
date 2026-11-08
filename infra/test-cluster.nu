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

# infra/test-cluster.nu
#
# The cluster an infra test may act on. A test that runs the agents on a cluster (`resolve.nu collect-scaffolds`,
# `resolve.nu check`'s agent checks) or applies to one (the in-cluster registry's storage chart) acts on whatever its
# KUBECONFIG names. The ambient KUBECONFIG is whatever cluster the shell last pointed at, so a test never uses it: it
# acts only on the cluster POLAR_TEST_CLUSTER_KUBECONFIG names, and skips, saying so, when that is not set.
#
#   POLAR_TEST_CLUSTER_KUBECONFIG=<kubeconfig> nu infra/tests/resolve.nu
#
# `pin` makes that the KUBECONFIG of the test process and of everything it runs. Without the opt-in it points
# KUBECONFIG at NO_CLUSTER, a path that cannot exist (under /dev/null), so a kubectl the test reaches by mistake fails at
# once on loading its configuration instead of falling back to ~/.kube/config. Tested in infra/tests/test-cluster.nu.

# The variable a test reads for the cluster it may act on: the path of that cluster's kubeconfig.
export const OPT_IN = "POLAR_TEST_CLUSTER_KUBECONFIG"

# What KUBECONFIG is set to when no cluster is named: kubectl refuses to load it ("not a directory").
export const NO_CLUSTER = "/dev/null/polar-tests-name-no-cluster"

# The kubeconfig the tests were given, expanded, or null when POLAR_TEST_CLUSTER_KUBECONFIG is unset or empty. One that
# names no file is an error: the run was meant for a cluster, and skipping would hide that it never reached it.
export def named [] {
    let given = ($env | get -o $OPT_IN | default "" | into string | str trim)
    if ($given | is-empty) { return null }
    let path = ($given | path expand)
    if not ($path | path exists) {
        error make --unspanned { msg: $"($OPT_IN) names ($given), which does not exist" }
    }
    $path
}

# Sets KUBECONFIG, for this process and what it runs, to the cluster the tests were given, or to NO_CLUSTER. Returns the
# kubeconfig, or null when no cluster was named.
export def --env pin [] {
    let cluster = (named)
    $env.KUBECONFIG = ($cluster | default $NO_CLUSTER)
    $cluster
}

# The line a test prints when it skips for want of a named cluster.
export def skip_line [what: string] {
    $"  skip  ($what): needs a cluster; set ($OPT_IN) to its kubeconfig \(the ambient KUBECONFIG is never used\)"
}
