#!/usr/bin/env nu
# infra/layers/3-workloads/agents/provenance/main.nu

def main [context_nuon: string] {
    let context    = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir
    let overrides  = $context.overrides
    let neo4j_addr = $context.neo4jBoltAddr

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "let v = " + $values_path + " in let o = (" + $overrides + ").provenance in v // { imagePullSecrets = o.imagePullSecrets, linker = v.linker // o.linker, resolver = v.resolver // o.resolver }"
    let tls_secret  = "provenance-agent-tls"

    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = (" + $merged + ") in " + $chart_dir + "/agent-cert.dhall v.tls"
    $expr | save --force $tmp
    print $"  rendering agent-cert.dhall -> provenance-agent-cert.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "provenance-agent-cert.yaml")
    rm $tmp

    let base = "let v = (" + $merged + ") in "
    let tls  = ", tlsSecretName = \"" + $tls_secret + "\", proxyCACert = v.proxyCACert }"
    let neo  = ", neo4jBoltAddr = \"" + $neo4j_addr + "\""

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/linker.dhall { name = v.linker.name, image = v.linker.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets" + $neo + $tls
    $expr | save --force $tmp
    print $"  rendering linker.dhall -> provenance-linker.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "provenance-linker.yaml")
    rm $tmp

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/resolver.dhall { name = v.resolver.name, image = v.resolver.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets" + $tls
    $expr | save --force $tmp
    print $"  rendering resolver.dhall -> provenance-resolver.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "provenance-resolver.yaml")
    rm $tmp

    print $"  provenance: done"
}
