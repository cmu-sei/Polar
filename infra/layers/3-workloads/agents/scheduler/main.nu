#!/usr/bin/env nu
# infra/layers/3-workloads/agents/scheduler/main.nu

def main [context_nuon: string] {
    let context    = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir
    let overrides  = $context.overrides
    let neo4j_addr = $context.neo4jBoltAddr
    let remote_url = $context.remoteUrl

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "let v = " + $values_path + " in let o = (" + $overrides + ").scheduler in v // { imagePullSecrets = o.imagePullSecrets, observer = v.observer // o.observer, processor = v.processor // o.processor }"
    let tls_secret  = "scheduler-agent-tls"

    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = (" + $merged + ") in " + $chart_dir + "/agent-cert.dhall v.tls"
    $expr | save --force $tmp
    print $"  rendering agent-cert.dhall -> scheduler-agent-cert.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "scheduler-agent-cert.yaml")
    rm $tmp

    let base = "let v = (" + $merged + ") in "
    let tls  = ", tlsSecretName = \"" + $tls_secret + "\", proxyCACert = v.proxyCACert }"
    let neo  = ", neo4jBoltAddr = \"" + $neo4j_addr + "\""

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/observer.dhall { name = v.observer.name, image = v.observer.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, syncInterval = v.observer.syncInterval, localPath = v.observer.localPath, remoteUrl = \"" + $remote_url + "\"" + $tls
    $expr | save --force $tmp
    print $"  rendering observer.dhall -> scheduler-observer.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "scheduler-observer.yaml")
    rm $tmp

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/processor.dhall { name = v.processor.name, image = v.processor.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets" + $neo + $tls
    $expr | save --force $tmp
    print $"  rendering processor.dhall -> scheduler-processor.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "scheduler-processor.yaml")
    rm $tmp

    print $"  scheduler: done"
}
