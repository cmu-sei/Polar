#!/usr/bin/env nu

# ACE Tool
#
# Copyright 2026 Carnegie Mellon University.
#
# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
# FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
# EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
# PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
# MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
# TRADEMARK, OR COPYRIGHT INFRINGEMENT.
#
# Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
# for full terms.
#
# [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
# distribution.  Please see Copyright notice for non-US Government use and distribution.
#
# This Software includes and/or makes use of Third-Party Software each subject to its own license.
#
# DM26-0670

# infra/layers/2-services/neo4j/main.nu

use ../../../dhall-render.nu [dhall_yaml]

def main [context_nuon: string] {
    let context    = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir
    let overrides  = $context.overrides
    let target_dir = $context.target_dir

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "((" + $values_path + ") // (" + $overrides + ").neo4j)"

    # TLS is always enabled via cert-issuer — always use the SSL conf
    let conf_file    = ($target_dir | path join "conf/neo4j-ssl.conf")
    # It goes into a Dhall string: a backslash starts an escape, a quote ends the string, ${ starts an
    # interpolation and a newline is not allowed. The backslash is done first so the ones added are not doubled.
    let conf_content = (
        open $conf_file
        | str replace --all "\\" "\\\\"
        | str replace --all '"' '\"'
        | str replace --all '${' '\${'
        | str replace --all "\n" "\\n"
    )

    # ── ConfigMap: neo4j.conf ─────────────────────────────────────────────────
    let tmp  = (mktemp --suffix ".dhall")
    let expr = (
        $chart_dir + "/configmap.dhall { namespace = (" + $merged + ").namespace, configContent = \""
        + $conf_content + "\" }"
    )
    $expr | save --force $tmp
    print $"  rendering configmap.dhall -> neo4j-configmap.yaml"
    dhall_yaml $tmp ["--documents"] | save --force ($output_dir | path join "neo4j-configmap.yaml")
    rm $tmp

    # ── ConfigMap: init script ────────────────────────────────────────────────
    print $"  rendering script-configmap.dhall -> neo4j-init-script-configmap.yaml"
    dhall_yaml ($chart_dir | path join "script-configmap.dhall") ["--documents"]
    | save --force ($output_dir | path join "neo4j-init-script-configmap.yaml")

    # ── PVCs ──────────────────────────────────────────────────────────────────
    let tmp  = (mktemp --suffix ".dhall")
    let expr = (
        "let v = " + $merged + " in "
        + $chart_dir + "/pvcs.dhall {"
        + "  namespace = v.namespace"
        + ", volumes   = { data = v.volumes.data, logs = v.volumes.logs, certs = v.volumes.certs }"
        + "}"
    )
    $expr | save --force $tmp
    print $"  rendering pvcs.dhall -> neo4j-pvcs.yaml"
    dhall_yaml $tmp ["--documents"] | save --force ($output_dir | path join "neo4j-pvcs.yaml")
    rm $tmp

    # ── Service ───────────────────────────────────────────────────────────────
    let tmp  = (mktemp --suffix ".dhall")
    let expr = (
        "let v = " + $merged + " in "
        + $chart_dir + "/service.dhall {"
        + "  ports = v.ports"
        + "}"
    )
    $expr | save --force $tmp
    print $"  rendering service.dhall -> neo4j-service.yaml"
    dhall_yaml $tmp ["--documents"] | save --force ($output_dir | path join "neo4j-service.yaml")
    rm $tmp

    # ── StatefulSet ───────────────────────────────────────────────────────────
    let tmp  = (mktemp --suffix ".dhall")
    let expr = (
        "let v = " + $merged + " in "
        + $chart_dir + "/statefulset.dhall {"
        + "  name               = v.name"
        + ", image              = v.image"
        + ", imagePullPolicy    = v.imagePullPolicy"
        + ", namespace          = v.namespace"
        + ", serviceAccountName = v.serviceAccountName"
        + ", configVolume       = v.configVolume"
        + ", ports              = v.ports"
        + ", config             = v.config"
        + ", volumes            = v.volumes"
        + ", credentialsObserverUrl = \"" + $context.credentialsObserverUrl + "\""
        + ", polarInitImage     = \"" + $context.polarInitImage + "\""
        + ", nuInitImage        = v.nuInitImage"
        + ", certIssuerAudience = v.certIssuerAudience"
        + ", neo4jSans          = v.neo4jSans"
        + "}"
    )
    $expr | save --force $tmp
    print $"  rendering statefulset.dhall -> neo4j-statefulset.yaml"
    dhall_yaml $tmp ["--documents"] | save --force ($output_dir | path join "neo4j-statefulset.yaml")
    rm $tmp

    print $"  neo4j: done"
}
