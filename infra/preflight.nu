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

# infra/preflight.nu
#
# What up.nu's first step asks of the cluster and of this machine, so that a cluster or a tool too old for Polar is
# found out at once and not steps later. The decisions are pure and tested (infra/tests/preflight.nu); up.nu asks.

# Every workload restarts in place by a container restart rule, RestartAllContainers, on by default from 1.36
# (docs/architecture/deployments.md); an older API server refuses the pods.
const KUBERNETES_MIN = [1 36]

# Why the cluster's Kubernetes, `kubectl version`'s serverVersion.gitVersion ("v1.37.1", "v1.31.12-eks.1"), is too old
# for Polar, or null when it is new enough. A version that cannot be read is a problem, not a pass.
export def kubernetes_problem [git_version: string]: nothing -> any {
    let found = ($git_version | parse --regex '^v?(?<major>\d+)\.(?<minor>\d+)')
    if ($found | is-empty) {
        return $"cannot read the cluster's Kubernetes version \('($git_version)'\); Polar needs 1.36 or later"
    }
    let major = ($found.0.major | into int)
    let minor = ($found.0.minor | into int)
    if $major > $KUBERNETES_MIN.0 or ($major == $KUBERNETES_MIN.0 and $minor >= $KUBERNETES_MIN.1) { return null }
    ([
        $"this cluster runs Kubernetes ($major).($minor); Polar needs 1.36 or later, whose pods restart in place by a"
        "container restart rule \(RestartAllContainers, on by default from 1.36\), which an older cluster refuses."
        "On minikube: v1.39.0 or later \(minikube version\), then a cluster of 1.36 or later:"
        "minikube start --kubernetes-version=v1.37.0, or minikube delete and minikube start"
    ] | str join " ")
}

# Why this machine's openssl, `openssl version` ("" when there is none), cannot do what up.nu asks of it (`x509 -ext`,
# OpenSSL 1.1.1 and later), or null when it can. macOS's own is LibreSSL, which has no -ext.
export def openssl_problem [version: string]: nothing -> any {
    let fix = "brew install openssl@3, then export PATH=\"$(brew --prefix openssl@3)/bin:$PATH\" (or run this in the dev container, bin/polar-dev)"
    let found = ($version | parse --regex '^OpenSSL (?<major>\d+)\.(?<minor>\d+)\.(?<patch>\d+)(?<letter>[a-z]*)')
    if ($found | is-empty) {
        let which = if ($version | str trim | is-empty) { "no openssl on the PATH" } else { $"the openssl on the PATH is ($version | str trim)" }
        return $"($which); up.nu needs OpenSSL 1.1.1 or later \(it reads certificates with openssl x509 -ext\): ($fix)"
    }
    let v = ($found.0 | update major { into int } | update minor { into int } | update patch { into int })
    let enough = ($v.major > 1 or ($v.major == 1 and ($v.minor > 1 or ($v.minor == 1 and $v.patch >= 1))))
    if $enough { null } else {
        $"the openssl on the PATH is ($version | str trim); up.nu needs OpenSSL 1.1.1 or later \(openssl x509 -ext\): ($fix)"
    }
}
