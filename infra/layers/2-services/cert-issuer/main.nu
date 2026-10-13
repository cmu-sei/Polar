#!/usr/bin/env nu
# infra/layers/2-services/cert-issuer/main.nu

def main [context_nuon: string] {
    let context    = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir
    let overrides  = $context.overrides

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    # The floor's entries come from the catalog (infra/resolve.nu floor); the chart's own list holds what the
    # catalog does not describe (Neo4j), and goes after them.
    let entry_type = "{ workload : Text, certType : Text, names : List Text, vouchedOnly : Bool }"
    let entries = ($context.floor | each {|e|
        let names = ($e.names | each {|n| $"\"($n)\"" } | str join ", ")
        let vouched = (if $e.vouchedOnly { "True" } else { "False" })
        $"{ workload = \"($e.workload)\", certType = \"($e.certType)\", names = [ ($names) ] : List Text, vouchedOnly = ($vouched) }"
    } | str join ", ")
    # Neo4j is not a catalog workload: its entry is its chart's service account and namespace with the names
    # its init container asks for, which the target sets (`neo4jSans`). It goes through the credentials agent.
    let neo4j_values = ($chart_dir | path dirname | path join "neo4j/values.dhall")
    let neo4j = $"\(($neo4j_values)\) // \(($overrides)\).neo4j"
    let neo4j_entry = $"{ workload = \"system:serviceaccount:\" ++ \(($neo4j)\).namespace ++ \":\" ++ \(($neo4j)\).serviceAccountName, certType = \"SERVER\", names = \(($neo4j)\).neo4jSans, vouchedOnly = True }"
    let entries = (if ($entries | is-empty) { $neo4j_entry } else { $"($entries), ($neo4j_entry)" })
    let catalog_floor = $"[ ($entries) ] : List ($entry_type)"
    let enforce     = (if $context.floorEnforced { "True" } else { "False" })
    let merged      = "(((" + $values_path + ") // (" + $overrides + ").certIssuer) // { mtlsBootstrap = (" + $catalog_floor + ") # ((" + $values_path + ").mtlsBootstrap), mtlsEnforceBootstrap = " + $enforce + " })"

    # PVC — must be applied before Deployment
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/pvc.dhall { name = v.name, caVolumeName = v.caVolumeName, caStorageClass = v.caStorageClass, caStorageSize = v.caStorageSize }"
    $expr | save --force $tmp
    print $"  rendering pvc.dhall -> cert-issuer-pvc.yaml"
    dhall-to-yaml --file $tmp | save --force ($output_dir | path join "cert-issuer-pvc.yaml")
    rm $tmp

    # ConfigMap — config JSON rendered from target OIDC values
    let tmp  = (mktemp --suffix ".dhall")
    let identity_overrides_dhall = ($context.identityLifetimeOverrides
    | each {|o| $"{ identity = \"($o.identity)\", lifetimeSecs = ($o.lifetimeSecs) }" }
    | str join ", ")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/configmap.dhall { name = v.name, port = v.port, caCertPath = v.caCertPath, caKeyPath = v.caKeyPath, issuers = v.issuers, defaultLifetimeSecs = v.defaultLifetimeSecs, serverLifetimeSecs = v.serverLifetimeSecs, userLifetimeSecs = v.userLifetimeSecs, mtlsServerIdentity = v.mtlsServerIdentity, mtlsInternalCallers = v.mtlsInternalCallers, mtlsBootstrap = v.mtlsBootstrap, mtlsEnforceBootstrap = v.mtlsEnforceBootstrap, caProvisioning = v.caProvisioning, identityLifetimeOverrides = [ " + $identity_overrides_dhall + " ] }"
    $expr | save --force $tmp
    print $"  rendering configmap.dhall -> cert-issuer-configmap.yaml"
    dhall-to-yaml --file $tmp | save --force ($output_dir | path join "cert-issuer-configmap.yaml")
    rm $tmp

    # Service
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/service.dhall { name = v.name, port = v.port }"
    $expr | save --force $tmp
    print $"  rendering service.dhall -> cert-issuer-service.yaml"
    dhall-to-yaml --file $tmp | save --force ($output_dir | path join "cert-issuer-service.yaml")
    rm $tmp

    # Deployment
    let tmp  = (mktemp --suffix ".dhall")
    let expr = "let v = " + $merged + " in " + $chart_dir + "/deployment.dhall { name = v.name, image = v.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, port = v.port, caVolumeName = v.caVolumeName, caCertPath = v.caCertPath, caKeyPath = v.caKeyPath, caProvisioning = v.caProvisioning }"
    $expr | save --force $tmp
    print $"  rendering deployment.dhall -> cert-issuer-deployment.yaml"
    dhall-to-yaml --file $tmp | save --force ($output_dir | path join "cert-issuer-deployment.yaml")
    rm $tmp

    print $"  cert-issuer: done"
}
