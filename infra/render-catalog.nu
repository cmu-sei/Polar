#!/usr/bin/env nu
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

# Which template renders which workload kind. A kind with no entry here is a
# custom chart, not a catalog workload.
const TEMPLATES = { agent: "agent" }

def fail [msg: string] {
    print -e $"  ERROR: ($msg)"
    exit 1
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
        let rendered = ($expr | dhall-to-yaml --quoted --documents | complete)
        if $rendered.exit_code != 0 {
            fail $"($name): dhall-to-yaml failed\n($rendered.stderr)"
        }

        let out = ($output_dir | path join $"($name).yaml")
        $rendered.stdout | save --force $out
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
            let res_out = ($res_expr | dhall-to-yaml --quoted --documents | complete)
            if $res_out.exit_code != 0 {
                fail $"($name): dhall-to-yaml failed on resources\n($res_out.stderr)"
            }
            $res_out.stdout | save --force ($output_dir | path join $"($name)-resources.yaml")
            print $"  rendering resources.dhall -> ($name)-resources.yaml"
        }
    }
}
