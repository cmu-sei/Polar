#!/usr/bin/env nu
def main [context_nuon: string] {
    let context           = ($context_nuon | from nuon)
    let chart_dir         = $context.chart_dir
    let output_dir        = $context.output_dir
    let overrides         = $context.overrides
    let neo4j_addr        = $context.neo4jBoltAddr
    let cert_issuer_url   = $context.certIssuerUrl
    let cert_client_image = $context.certClientImage
    let sa_audience       = $context.certIssuerAudience

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "let v = " + $values_path + " in let o = (" + $overrides + ").build in v // { imagePullSecrets = o.imagePullSecrets, orchestrator = v.orchestrator // o.orchestrator, processor = v.processor // o.processor }"
    let base        = "let v = (" + $merged + ") in "
    let cert_args   = ", certClientImage = \"" + $cert_client_image + "\", certIssuerUrl = \"" + $cert_issuer_url + "\", saTokenAudience = \"" + $sa_audience + "\", proxyCACert = v.proxyCACert }"
    let neo         = ", neo4jBoltAddr = \"" + $neo4j_addr + "\""

    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = (" + $merged + ") in " + $chart_dir + "/rbac.dhall { orchestrator = { serviceAccountName = v.orchestrator.serviceAccountName, secretName = v.orchestrator.secretName } }"
    $expr | save --force $tmp
    print $"  rendering rbac.dhall -> build-agent-rbac.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "build-agent-rbac.yaml")
    rm $tmp

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/orchestrator.dhall { name = v.orchestrator.name, image = v.orchestrator.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, serviceAccountName = v.orchestrator.serviceAccountName, secretName = v.orchestrator.secretName" + $cert_args
    $expr | save --force $tmp
    print $"  rendering orchestrator.dhall -> build-orchestrator.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "build-orchestrator.yaml")
    rm $tmp

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/processor.dhall { name = v.processor.name, image = v.processor.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets" + $neo + $cert_args
    $expr | save --force $tmp
    print $"  rendering processor.dhall -> build-processor.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "build-processor.yaml")
    rm $tmp

    print $"  build: done"
}
