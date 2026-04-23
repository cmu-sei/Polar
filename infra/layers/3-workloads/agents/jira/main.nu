#!/usr/bin/env nu
# infra/layers/3-workloads/agents/jira/main.nu

def main [context_nuon: string] {
    let context    = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir
    let overrides  = $context.overrides
    let neo4j_addr = $context.neo4jBoltAddr

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "let v = " + $values_path + " in let o = (" + $overrides + ").jira in v // { imagePullSecrets = o.imagePullSecrets, observer = v.observer // o.observer, processor = v.processor // o.processor }"
    let tls_secret  = "jira-agent-tls"

    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = (" + $merged + ") in " + $chart_dir + "/agent-cert.dhall v.tls"
    $expr | save --force $tmp
    print $"  rendering agent-cert.dhall -> jira-agent-cert.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "jira-agent-cert.yaml")
    rm $tmp

    let base = "let v = (" + $merged + ") in "
    let tls  = ", tlsSecretName = \"" + $tls_secret + "\", proxyCACert = v.proxyCACert }"
    let neo  = ", neo4jBoltAddr = \"" + $neo4j_addr + "\""

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/observer.dhall { name = v.observer.name, image = v.observer.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, jiraUrl = v.observer.jiraUrl" + $tls
    $expr | save --force $tmp
    print $"  rendering observer.dhall -> jira-observer.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "jira-observer.yaml")
    rm $tmp

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/processor.dhall { name = v.processor.name, image = v.processor.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets" + $neo + $tls
    $expr | save --force $tmp
    print $"  rendering processor.dhall -> jira-processor.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "jira-processor.yaml")
    rm $tmp

    print $"  jira: done"
}
