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

    # Deep merge: merge observer and consumer sub-records individually
    # so image overrides don't wipe out other observer/consumer fields.
    let merged = "let v = " + $values_path + " in let o = (" + $overrides + ").gitlab in v // { imagePullSecrets = o.imagePullSecrets, observer = v.observer // o.observer, consumer = v.consumer // o.consumer }"

    # Agent certificate
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = (" + $merged + ") in " + $chart_dir + "/agent-cert.dhall v.tls"
    $expr | save --force $tmp
    print $"  rendering agent-cert.dhall -> gitlab-agent-cert.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "gitlab-agent-cert.yaml")
    rm $tmp

    # Deployment
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = (" + $merged + ") in " + $chart_dir + "/deployment.dhall (v // { neo4jBoltAddr = \"" + $neo4j_addr + "\", neo4jCAMountPath = \"/etc/neo4j-ca/ca.pem\" })"
    $expr | save --force $tmp
    print $"  rendering deployment.dhall -> gitlab-agents.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "gitlab-agents.yaml")
    rm $tmp

    print $"  gitlab: done"
}
