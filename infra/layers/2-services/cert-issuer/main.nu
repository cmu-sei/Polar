#!/usr/bin/env nu
# infra/layers/2-services/cert-issuer/main.nu

def main [context_nuon: string] {
    let context    = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir
    let overrides  = $context.overrides

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "((" + $values_path + ") // (" + $overrides + ").certIssuer)"

    # PVC — must be applied before Deployment
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/pvc.dhall { name = v.name, caVolumeName = v.caVolumeName, caStorageClass = v.caStorageClass, caStorageSize = v.caStorageSize }"
    $expr | save --force $tmp
    print $"  rendering pvc.dhall -> cert-issuer-pvc.yaml"
    dhall-to-yaml --file $tmp | save --force ($output_dir | path join "cert-issuer-pvc.yaml")
    rm $tmp

    # ConfigMap — config JSON rendered from target OIDC values
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/configmap.dhall { name = v.name, port = v.port, caCertPath = v.caCertPath, caKeyPath = v.caKeyPath, oidcIssuerUrl = v.oidcIssuerUrl, oidcAudience = v.oidcAudience, oidcJwksUri = v.oidcJwksUri }"
    $expr | save --force $tmp
    print $"  rendering configmap.dhall -> cert-issuer-configmap.yaml"
    dhall-to-yaml --file $tmp | save --force ($output_dir | path join "cert-issuer-configmap.yaml")
    rm $tmp

    # Service
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/service.dhall { name = v.name, port = v.port }"
    $expr | save --force $tmp
    print $"  rendering service.dhall -> cert-issuer-service.yaml"
    dhall-to-yaml --file $tmp | save --force ($output_dir | path join "cert-issuer-service.yaml")
    rm $tmp

    # Deployment
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/deployment.dhall { name = v.name, image = v.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, port = v.port, caVolumeName = v.caVolumeName, caCertPath = v.caCertPath, caKeyPath = v.caKeyPath }"
    $expr | save --force $tmp
    print $"  rendering deployment.dhall -> cert-issuer-deployment.yaml"
    dhall-to-yaml --file $tmp | save --force ($output_dir | path join "cert-issuer-deployment.yaml")
    rm $tmp

    print $"  cert-issuer: done"
}
