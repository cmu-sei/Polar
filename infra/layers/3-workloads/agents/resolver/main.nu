#!/usr/bin/env nu
def main [context_nuon: string] {
    let context           = ($context_nuon | from nuon)
    let chart_dir         = $context.chart_dir
    let output_dir        = $context.output_dir
    let overrides         = $context.overrides
    let cert_issuer_url   = $context.certIssuerUrl
    let polar_init_image  = $context.polarInitImage
    let sa_audience       = $context.certIssuerAudience

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "let v = " + $values_path + " in let o = (" + $overrides + ").resolver in v // { imagePullSecrets = o.imagePullSecrets, resolver = v.resolver // o.resolver }"
    let base        = "let v = (" + $merged + ") in "
    let cert_args   = ", polarInitImage = \"" + $polar_init_image + "\", certIssuerUrl = \"" + $cert_issuer_url + "\", saTokenAudience = \"" + $sa_audience + "\", proxyCACert = v.proxyCACert }"

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/resolver.dhall { name = v.resolver.name, image = v.resolver.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets" + $cert_args
    $expr | save --force $tmp
    print $"  rendering resolver.dhall -> resolver.yaml"
    dhall-to-yaml --documents --quoted --file $tmp | save --force ($output_dir | path join "resolver.yaml")
    rm $tmp

    print $"  resolver: done"
}
