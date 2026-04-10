#!/usr/bin/env nu
# infra/layers/2-services/cassini/main.nu

def main [context_nuon: string] {
    let context    = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir
    let overrides  = $context.overrides
    let jaeger_dns = $context.jaegerDNSName

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "((" + $values_path + ") // (" + $overrides + ").cassini)"

    # Server certificate — needs tls field
    let tmp  = (mktemp --suffix ".dhall")
    let expr = $chart_dir + "/server-cert.dhall (" + $merged + ").tls"
    $expr | save --force $tmp
    print $"  rendering server-cert.dhall -> cassini-server-cert.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "cassini-server-cert.yaml")
    rm $tmp

    # Client certificate — fully static
    print $"  rendering client-cert.dhall -> cassini-client-cert.yaml"
    dhall-to-yaml --documents --file ($chart_dir | path join "client-cert.dhall") | save --force ($output_dir | path join "cassini-client-cert.yaml")

    # Deployment — project only the fields deployment.dhall expects
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/deployment.dhall { name = v.name, image = v.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, ports = v.ports, jaegerDNSName = \"" + $jaeger_dns + "\" }"
    $expr | save --force $tmp
    print $"  rendering deployment.dhall -> cassini.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "cassini.yaml")
    rm $tmp

    print $"  cassini: done"
}
