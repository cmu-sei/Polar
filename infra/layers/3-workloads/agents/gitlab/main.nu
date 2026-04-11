#!/usr/bin/env nu
# infra/layers/3-workloads/agents/gitlab/main.nu

def main [context_nuon: string] {
    let context    = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir
    let overrides  = $context.overrides
    let neo4j_addr = $context.neo4jBoltAddr

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "let v = " + $values_path + " in let o = (" + $overrides + ").gitlab in v // { imagePullSecrets = o.imagePullSecrets, observer = v.observer // o.observer, consumer = v.consumer // o.consumer }"
    let tls_secret  = "gitlab-agent-tls"

    # Agent certificate
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = (" + $merged + ") in " + $chart_dir + "/agent-cert.dhall v.tls"
    $expr | save --force $tmp
    print $"  rendering agent-cert.dhall -> gitlab-agent-cert.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "gitlab-agent-cert.yaml")
    rm $tmp

    # Observer deployment
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = (" + $merged + ") in " + $chart_dir + "/observer.dhall { name = v.observer.name, image = v.observer.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, endpoint = v.observer.endpoint, baseIntervalSecs = v.observer.baseIntervalSecs, tlsSecretName = \"" + $tls_secret + "\", proxyCACert = v.proxyCACert }"
    $expr | save --force $tmp
    print $"  rendering observer.dhall -> gitlab-observer.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "gitlab-observer.yaml")
    rm $tmp

    # Consumer deployment
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = (" + $merged + ") in " + $chart_dir + "/consumer.dhall { name = v.consumer.name, image = v.consumer.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, tlsSecretName = \"" + $tls_secret + "\", neo4jBoltAddr = \"" + $neo4j_addr + "\", proxyCACert = v.proxyCACert }"
    $expr | save --force $tmp
    print $"  rendering consumer.dhall -> gitlab-consumer.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "gitlab-consumer.yaml")
    rm $tmp

    print $"  gitlab: done"
}
