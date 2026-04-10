#!/usr/bin/env nu
# infra/layers/2-services/jaeger/main.nu

def main [context_nuon: string] {
    let context    = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir
    let overrides  = $context.overrides

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let render_path = ($chart_dir | path join "deployment.dhall")
    let tmp         = (mktemp --suffix ".dhall")

    let expr = $render_path + " ((" + $values_path + ") // (" + $overrides + ").jaeger)"
    $expr | save --force $tmp

    print $"  rendering jaeger deployment + service -> jaeger.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "jaeger.yaml")
    rm $tmp

    print $"  jaeger: done"
}
