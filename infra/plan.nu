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

# infra/plan.nu
#
# What up.nu decides to run, and what it forwards, for a target. Pure: it reads the target's own config.

# What the target deploys, by name: the chart directories, and the workloads the catalog renders, which
# have no chart and are known by their manifests in the apply order (`<name>.yaml`, and
# `<name>-resources.yaml` for the ones with resources).
export def deployed_charts [config: record] {
    let charts = ($config.charts | each {|c| $c | path dirname | path basename })
    let workloads = ($config.apply_order | each {|f| $f | str replace --regex '(-resources)?\.yaml$' '' })
    $charts | append $workloads | uniq
}

# The plan: one record per step, with whether it runs and why not.
export def build_plan [config: record, dev: bool] {
    let d = (deployed_charts $config)
    let order = $config.apply_order
    let ca_cm = ($config.cert_issuer_ca_configmap? | default "")
    let has_ci = ("cert-issuer" in $d)
    let has_ca_agent = ("credentials-agent" in $d)
    let has_neo4j = ("neo4j" in $d)
    let has_seed = ($has_ca_agent and ("polar-git-server" in $d) and ("polar-scheduler-observer" in $d))
    let has_core = ($has_ci or ("cassini" in $d) or $has_ca_agent or $has_neo4j)

    [
        { id: "preflight", title: "preflight", on: true, why: "" }
        { id: "agent-images", title: "agent images", on: $dev, why: "pass --dev to build the agents' images; without it the render asks the ones already in the local registry" }
        { id: "render", title: "render", on: true, why: "" }
        { id: "images", title: "images", on: $dev, why: "pass --dev to build and load images into the local registry" }
        { id: "namespaces", title: "namespaces", on: ("namespaces.yaml" in $order), why: "target has no namespaces.yaml in its apply order" }
        {
            id: "ca"
            title: "cluster CA"
            on: ($has_ci and ($ca_cm | is-not-empty))
            why: (if not $has_ci { "target does not deploy cert-issuer" } else { "target does not use a provisioned CA (cert_issuer_ca_configmap is empty)" })
        }
        { id: "infra", title: "secrets and infrastructure", on: true, why: "" }
        { id: "pods", title: "waiting for core services", on: $has_core, why: "target deploys none of cert-issuer, cassini, credentials-agent, neo4j" }
        { id: "forwards", title: "port-forwards", on: ($has_ca_agent or $has_neo4j), why: "target deploys neither credentials-agent nor neo4j" }
        { id: "admin", title: "admin flow", on: $has_ca_agent, why: "target does not deploy credentials-agent" }
        {
            id: "seed"
            title: "seed the schedules repository"
            on: $has_seed
            why: "target does not deploy all of credentials-agent, polar-git-server and polar-scheduler-observer"
        }
        {
            id: "browser"
            title: "browser certificate and Neo4j password"
            on: ($has_ca_agent and $has_neo4j)
            why: (if not $has_neo4j { "target does not deploy neo4j" } else { "no credentials-agent, so no user certificate to import" })
        }
    ]
}

# Port-forwards needed by the admin and browser steps, and only those.
export def build_forwards [config: record] {
    let d = (deployed_charts $config)
    mut f = []
    if "credentials-agent" in $d {
        $f = ($f | append { name: "observer", svc: "credentials-agent-observer" })
    }
    if "neo4j" in $d {
        $f = ($f | append { name: "neo4j", svc: "polar-db-svc" })
    }
    # The seed pushes to the git server from this machine.
    if ("credentials-agent" in $d) and ("polar-git-server" in $d) and ("polar-scheduler-observer" in $d) {
        $f = ($f | append { name: "git", svc: "polar-git-server" })
    }
    $f
}
