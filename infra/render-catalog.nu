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

# infra/render-catalog.nu
#
# Renders every workload in the catalog (infra/config/workloads/*.nuon) into
# <output_dir>/<name>.yaml. render.nu calls this after the charts.
#
#   nu infra/render-catalog.nu local-minimal manifests
#   nu infra/render-catalog.nu local-minimal manifests --only [kube-observer]
#
# Per workload:
#   resolve.nu json <name>     the resolved record (pure; touches nothing)
#   json-to-dhall              typechecks it against schema/agent-spec.dhall
#   dhall-to-yaml              applies schema/agent.dhall to it
# and, when the workload's capabilities ask for objects alongside it (RBAC,
# token Secrets), the same again with `resolve.nu resources <name>`,
# schema/resources-spec.dhall and schema/resources.dhall, into
# <name>-resources.yaml.
# Same flags as the charts' own rendering, so the output is formatted alike.

const INFRA = (path self | path dirname)

use cache.nu [cached_text, content_key, dir_digest]

# Which template renders which workload kind. A kind with no entry here is a
# custom chart, not a catalog workload. `ca` is cert-issuer's alone: it is the
# issuer, so its pod has no polar-init (schema/ca.dhall).
const TEMPLATES = { agent: "agent", service: "agent", ca: "ca" }

def fail [msg: string] {
    print -e $"  ERROR: ($msg)"
    exit 1
}

# Dhall's rendering of `expr`: read back when the same inputs were rendered before, rendered and kept when not.
# The key covers the expression (the typed spec, and the template it is applied to), every file in infra/schema
# (a deliberate over-approximation of what the templates import: any edit there is a different key), and the
# version of dhall-to-yaml. The remote imports are pinned by hash, so they cannot differ under the same key.
def render_yaml [name: string, expr: string, schema_key: string, what: string] {
    let key = (content_key [$schema_key $expr])
    try {
        cached_text "render" $key {||
            let r = ($expr | dhall-to-yaml --quoted --documents | complete)
            if $r.exit_code != 0 { error make { msg: $r.stderr } }
            $r.stdout
        }
    } catch {|e|
        fail $"($name): dhall-to-yaml failed($what)\n($e.msg)"
    }
}

def main [
    target: string
    output_dir: string
    --only: list<string> = []
] {
    for tool in [json-to-dhall dhall-to-yaml] {
        if (which $tool | is-empty) { fail $"($tool) is not on PATH \(it comes with the dhall-json package\)" }
    }

    let resolver = ($INFRA | path join "resolve.nu")
    let names = (nu $resolver names --target $target | from json)
    let selected = if ($only | is-empty) { $names } else { $names | where {|n| $n in $only } }

    mkdir $output_dir

    let schema_key = (content_key [(dir_digest ($INFRA | path join "schema") "*.dhall") (^dhall-to-yaml --version | str trim)])

    for name in $selected {
        let resolved = (nu $resolver json $name --target $target | from json)
        let template = ($TEMPLATES | get -o $resolved.kind)
        if $template == null {
            fail $"($name): no template for kind '($resolved.kind)' \(known: ($TEMPLATES | columns | str join ', ')\)"
        }

        let spec_file = ($INFRA | path join $"schema/($template)-spec.dhall")
        let template_file = ($INFRA | path join $"schema/($template).dhall")
        let schema = ("(" + $spec_file + ").Spec")

        let typed = ($resolved | to json | json-to-dhall $schema | complete)
        if $typed.exit_code != 0 {
            fail $"($name): the resolved record does not match ($template)-spec.dhall\n($typed.stderr)"
        }

        let expr = ("let spec = " + $typed.stdout + "\nin " + $template_file + " spec")
        let rendered = (render_yaml $name $expr $schema_key "")

        let out = ($output_dir | path join $"($name).yaml")
        $rendered | save --force $out
        print $"  rendering ($template).dhall -> ($name).yaml"

        # Objects that live alongside the workload, if its capabilities ask for any.
        let res = (nu $resolver resources $name --target $target | from json)
        if ($res.cluster_roles | is-not-empty) or ($res.sa_token_secrets | is-not-empty) or ($res.config_maps | is-not-empty) or ($res.pvcs | is-not-empty) or ($res.services | is-not-empty) {
            let res_schema = ("(" + ($INFRA | path join "schema/resources-spec.dhall") + ").Spec")
            let res_typed = ($res | to json | json-to-dhall $res_schema | complete)
            if $res_typed.exit_code != 0 {
                fail $"($name): resources do not match resources-spec.dhall\n($res_typed.stderr)"
            }
            let res_expr = ("let spec = " + $res_typed.stdout + "\nin " + ($INFRA | path join "schema/resources.dhall") + " spec")
            let res_out = (render_yaml $name $res_expr $schema_key " on resources")
            $res_out | save --force ($output_dir | path join $"($name)-resources.yaml")
            print $"  rendering resources.dhall -> ($name)-resources.yaml"
        }
    }
}
