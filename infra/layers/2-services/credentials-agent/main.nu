#!/usr/bin/env nu
# infra/layers/2-services/credentials-agent/main.nu

def main [context_nuon: string] {
    let context          = ($context_nuon | from nuon)
    let chart_dir        = $context.chart_dir
    let output_dir       = $context.output_dir
    let overrides        = $context.overrides
    let neo4j_addr       = $context.neo4jBoltAddr
    let cert_issuer_url  = $context.certIssuerUrl
    let polar_init_image = $context.polarInitImage
    let sa_audience      = $context.certIssuerAudience

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "((" + $values_path + ") // (" + $overrides + ").credentialsAgent)"

    # PVC -- must be applied before the Job
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/pvc.dhall { name = v.name, dataVolumeName = v.dataVolumeName, dataStorageClass = v.dataStorageClass, dataStorageSize = v.dataStorageSize }"
    $expr | save --force $tmp
    print $"  rendering pvc.dhall -> credentials-agent-pvc.yaml"
    dhall-to-yaml --file $tmp | save --force ($output_dir | path join "credentials-agent-pvc.yaml")
    rm $tmp

    # Job
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/deployment.dhall { name = v.name, image = v.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, polarInitImage = \"" + $polar_init_image + "\", certIssuerUrl = \"" + $cert_issuer_url + "\", saTokenAudience = \"" + $sa_audience + "\", neo4jBoltAddr = \"" + $neo4j_addr + "\", dataVolumeName = v.dataVolumeName }"
    $expr | save --force $tmp
    print $"  rendering deployment.dhall -> credentials-agent-job.yaml"
    dhall-to-yaml --documents --quoted --file $tmp | save --force ($output_dir | path join "credentials-agent-job.yaml")
    rm $tmp

    # Observer -- Job + Service
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/observer.dhall { name = v.observerName, image = v.observerImage, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, polarInitImage = \"" + $polar_init_image + "\", certIssuerUrl = \"" + $cert_issuer_url + "\", saTokenAudience = \"" + $sa_audience + "\", port = v.observerPort }"
    $expr | save --force $tmp
    print $"  rendering observer.dhall -> credentials-agent-observer.yaml"
    dhall-to-yaml --documents --quoted --file $tmp | save --force ($output_dir | path join "credentials-agent-observer.yaml")
    rm $tmp

    print $"  credentials-agent: done"
}
