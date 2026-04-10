#!/usr/bin/env nu
# infra/layers/2-services/neo4j/main.nu

def main [context_nuon: string] {
    let context    = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir
    let overrides  = $context.overrides
    let target_dir = $context.target_dir
    let enable_tls = $context.enable_tls

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "((" + $values_path + ") // (" + $overrides + ").neo4j)"

    # Read neo4j.conf from target conf directory
    let conf_file    = if $enable_tls { ($target_dir | path join "conf/neo4j-ssl.conf") } else { ($target_dir | path join "conf/neo4j-no-ssl.conf") }
    let conf_content = (open $conf_file | str replace --all "\\" "\\\\" | str replace --all '"' '\\"' | str replace --all "\n" "\\n")

    # ConfigMap: neo4j.conf
    let tmp  = (mktemp --suffix ".dhall")
    let expr = $chart_dir + "/configmap.dhall { namespace = (" + $merged + ").namespace, configContent = \"" + $conf_content + "\" }"
    $expr | save --force $tmp
    print $"  rendering configmap.dhall -> neo4j-configmap.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "neo4j-configmap.yaml")
    rm $tmp

    # ConfigMap: init script
    print $"  rendering script-configmap.dhall -> neo4j-init-script-configmap.yaml"
    dhall-to-yaml --documents --file ($chart_dir | path join "script-configmap.dhall") | save --force ($output_dir | path join "neo4j-init-script-configmap.yaml")

    # PVCs
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/pvcs.dhall { namespace = v.namespace, enableTls = v.enableTls, volumes = v.volumes }"
    $expr | save --force $tmp
    print $"  rendering pvcs.dhall -> neo4j-pvcs.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "neo4j-pvcs.yaml")
    rm $tmp

    # Service
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/service.dhall { enableTls = v.enableTls, ports = v.ports }"
    $expr | save --force $tmp
    print $"  rendering service.dhall -> neo4j-service.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "neo4j-service.yaml")
    rm $tmp

    # StatefulSet — project only the fields statefulset.dhall expects
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/statefulset.dhall { name = v.name, image = v.image, imagePullPolicy = v.imagePullPolicy, namespace = v.namespace, enableTls = v.enableTls, configVolume = v.configVolume, ports = v.ports, config = v.config, volumes = v.volumes }"
    $expr | save --force $tmp
    print $"  rendering statefulset.dhall -> neo4j-statefulset.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "neo4j-statefulset.yaml")
    rm $tmp

    print $"  neo4j: done"
}
