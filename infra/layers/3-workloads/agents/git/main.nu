#!/usr/bin/env nu
# infra/layers/3-workloads/agents/git/main.nu

def main [context_nuon: string] {
    let context    = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir
    let overrides  = $context.overrides
    let neo4j_addr = $context.neo4jBoltAddr

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "let v = " + $values_path + " in let o = (" + $overrides + ").git in v // { imagePullSecrets = o.imagePullSecrets, observer = v.observer // o.observer, consumer = v.consumer // o.consumer, scheduler = v.scheduler // o.scheduler }"
    let tls_secret  = "git-agent-tls"

    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = (" + $merged + ") in " + $chart_dir + "/agent-cert.dhall v.tls"
    $expr | save --force $tmp
    print $"  rendering agent-cert.dhall -> git-agent-cert.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "git-agent-cert.yaml")
    rm $tmp

    let base = "let v = (" + $merged + ") in "
    let tls  = ", tlsSecretName = \"" + $tls_secret + "\", proxyCACert = v.proxyCACert }"
    let neo  = ", neo4jBoltAddr = \"" + $neo4j_addr + "\""

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/observer.dhall { name = v.observer.name, image = v.observer.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets" + $tls
    $expr | save --force $tmp
    print $"  rendering observer.dhall -> git-observer.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "git-observer.yaml")
    rm $tmp

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/consumer.dhall { name = v.consumer.name, image = v.consumer.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets" + $neo + $tls
    $expr | save --force $tmp
    print $"  rendering consumer.dhall -> git-consumer.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "git-consumer.yaml")
    rm $tmp

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/scheduler.dhall { name = v.scheduler.name, image = v.scheduler.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets" + $neo + $tls
    $expr | save --force $tmp
    print $"  rendering scheduler.dhall -> git-scheduler.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "git-scheduler.yaml")
    rm $tmp

    print $"  git: done"
}
