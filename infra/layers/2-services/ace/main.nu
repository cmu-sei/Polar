#!/usr/bin/env nu
# infra/layers/2-services/ace/main.nu

def main [context_nuon: string] {
    let context           = ($context_nuon | from nuon)
    let chart_dir         = $context.chart_dir
    let output_dir        = $context.output_dir
    let overrides         = $context.overrides
    let neo4j_addr        = $context.neo4jBoltAddr
    let polar_init_image  = $context.polarInitImage
    let sa_audience       = $context.certIssuerAudience

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "let v = " + $values_path + " in let o = (" + $overrides + ").ace in v // o"
    let base        = "let v = (" + $merged + ") in "

    # ── RBAC (ServiceAccount) ───────────────────────────────────────────────
    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/rbac.dhall { serviceAccountName = v.serviceAccountName }"
    $expr | save --force $tmp
    print $"  rendering rbac.dhall -> ace-rbac.yaml"
    dhall-to-yaml --documents --quoted --file $tmp | save --force ($output_dir | path join "ace-rbac.yaml")
    rm $tmp

    # ── Job + Service ────────────────────────────────────────────────────────
    let tmp  = (mktemp --suffix ".dhall")
    let expr = (
        $base + $chart_dir + "/job.dhall {"
        + " name = v.name, image = v.image, imagePullPolicy = v.imagePullPolicy,"
        + " imagePullSecrets = v.imagePullSecrets, serviceAccountName = v.serviceAccountName,"
        + " serviceType = v.serviceType, port = v.port,"
        + " polarInitImage = \"" + $polar_init_image + "\","
        + " credentialsObserverUrl = \"" + $context.credentialsObserverUrl + "\","
        + " saTokenAudience = \"" + $sa_audience + "\","
        + " neo4jBoltAddr = \"" + $neo4j_addr + "\""
        + " }"
    )
    $expr | save --force $tmp
    print $"  rendering job.dhall -> ace.yaml"
    dhall-to-yaml --documents --quoted --file $tmp | save --force ($output_dir | path join "ace.yaml")
    rm $tmp

    print $"  ace: done"
}
