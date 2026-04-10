#!/usr/bin/env nu
# infra/layers/1-platform/infra/namespaces/main.nu
#
# Chart entrypoint for namespace definitions.
# Renders: polar and polar-db namespaces.
# Applied first in every target — all other resources depend on these.
#
# Called by render.nu with a context record:
#   {
#     chart_dir: string       # absolute path to this chart directory
#     output_dir: string      # absolute path to write rendered YAML into
#     overrides: string       # path to target overrides.dhall
#   }
#
# Writes:
#   <output_dir>/namespaces.yaml

def main [context_nuon: string] {
    let context   = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir

    mkdir $output_dir

    let src = ($chart_dir | path join "namespaces.dhall")
    let out = ($output_dir | path join "namespaces.yaml")
    print $"  rendering namespaces.dhall -> namespaces.yaml"
    dhall-to-yaml --documents --file $src | save --force $out

    print $"  namespaces: done"
}
