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
    let merged      = "let v = " + $values_path + " in let o = (" + $overrides + ").kube in v // { imagePullSecrets = o.imagePullSecrets, observer = v.observer // o.observer, consumer = v.consumer // o.consumer }"
    let base        = "let v = (" + $merged + ") in "
    let cert_args   = ", certClientImage = \"" + $cert_client_image + "\", certIssuerUrl = \"" + $cert_issuer_url + "\", saTokenAudience = \"" + $sa_audience + "\", proxyCACert = v.proxyCACert }"
    let neo         = ", neo4jBoltAddr = \"" + $neo4j_addr + "\""

    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = (" + $merged + ") in " + $chart_dir + "/rbac.dhall { observer = { serviceAccountName = v.observer.serviceAccountName, secretName = v.observer.secretName } }"
    $expr | save --force $tmp
    print $"  rendering rbac.dhall -> kube-agent-rbac.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "kube-agent-rbac.yaml")
    rm $tmp

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/observer.dhall { name = v.observer.name, image = v.observer.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, serviceAccountName = v.observer.serviceAccountName, secretName = v.observer.secretName" + $cert_args
    $expr | save --force $tmp
    print $"  rendering observer.dhall -> kube-observer.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "kube-observer.yaml")
    rm $tmp

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/consumer.dhall { name = v.consumer.name, image = v.consumer.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets" + $neo + $cert_args
    $expr | save --force $tmp
    print $"  rendering consumer.dhall -> kube-consumer.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "kube-consumer.yaml")
    rm $tmp

    print $"  kube: done"
}
