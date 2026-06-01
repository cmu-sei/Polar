#!/usr/bin/env nu
def main [context_nuon: string] {
    let context           = ($context_nuon | from nuon)
    let chart_dir         = $context.chart_dir
    let output_dir        = $context.output_dir
    let overrides         = $context.overrides
    let jaeger_dns        = $context.jaegerDNSName
    let cert_issuer_url   = $context.certIssuerUrl
    let cert_client_image = $context.certClientImage
    let sa_audience       = $context.certIssuerAudience

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "((" + $values_path + ") // (" + $overrides + ").cassini)"

    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/deployment.dhall { name = v.name, image = v.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, certClientImage = \"" + $cert_client_image + "\", certIssuerUrl = \"" + $cert_issuer_url + "\", saTokenAudience = \"" + $sa_audience + "\", ports = v.ports, jaegerDNSName = \"" + $jaeger_dns + "\" }"
    $expr | save --force $tmp
    print $"  rendering deployment.dhall -> cassini.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "cassini.yaml")
    rm $tmp

    print $"  cassini: done"
}
