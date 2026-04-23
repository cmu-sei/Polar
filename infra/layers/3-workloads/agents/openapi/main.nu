#!/usr/bin/env nu
# infra/layers/3-workloads/agents/openapi/main.nu

def main [context_nuon: string] {
    let context    = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir
    let overrides  = $context.overrides
    let neo4j_addr = $context.neo4jBoltAddr

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "let v = " + $values_path + " in let o = (" + $overrides + ").openapi in v // { imagePullSecrets = o.imagePullSecrets, observer = v.observer // o.observer, processor = v.processor // o.processor }"
    let tls_secret  = "openapi-agent-tls"

    # Agent certificate
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = (" + $merged + ") in " + $chart_dir + "/agent-cert.dhall v.tls"
    $expr | save --force $tmp
    print $"  rendering agent-cert.dhall -> openapi-agent-cert.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "openapi-agent-cert.yaml")
    rm $tmp

    # Observer deployment
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = (" + $merged + ") in " + $chart_dir + "/observer.dhall { name = v.observer.name, image = v.observer.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, tlsSecretName = \"" + $tls_secret + "\", openapiEndpoint = v.observer.openapiEndpoint }"
    $expr | save --force $tmp
    print $"  rendering observer.dhall -> openapi-observer.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "openapi-observer.yaml")
    rm $tmp

    # Processor deployment
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = (" + $merged + ") in " + $chart_dir + "/processor.dhall { name = v.processor.name, image = v.processor.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, tlsSecretName = \"" + $tls_secret + "\", neo4jBoltAddr = \"" + $neo4j_addr + "\" }"
    $expr | save --force $tmp
    print $"  rendering processor.dhall -> openapi-processor.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "openapi-processor.yaml")
    rm $tmp

    print $"  openapi: done"
}
