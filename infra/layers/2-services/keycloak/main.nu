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

# infra/layers/2-services/keycloak/main.nu

def main [context_nuon: string] {
    let context    = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir
    let overrides  = $context.overrides

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "((" + $values_path + ") // (" + $overrides + ").keycloak)"

    # -- Service --
    let tmp  = (mktemp --suffix ".dhall")
    let expr = (
        "let v = " + $merged + " in "
        + $chart_dir + "/service.dhall {"
        + "  name        = v.name"
        + ", serviceType = v.serviceType"
        + ", ports       = v.ports"
        + "}"
    )
    $expr | save --force $tmp
    print $"  rendering service.dhall -> keycloak-service.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "keycloak-service.yaml")
    rm $tmp

    # -- Deployment --
    let tmp  = (mktemp --suffix ".dhall")
    let expr = (
        "let v = " + $merged + " in "
        + $chart_dir + "/deployment.dhall {"
        + "  name                = v.name"
        + ", image               = v.image"
        + ", imagePullPolicy     = v.imagePullPolicy"
        + ", ports               = v.ports"
        + ", dbHost              = v.dbHost"
        + ", dbName              = v.dbName"
        + ", dbUser              = v.dbUser"
        + ", adminUserSecret     = { name = \"keycloak-admin-secret\", key = \"username\" }"
        + ", adminPasswordSecret = { name = \"keycloak-admin-secret\", key = \"password\" }"
        + ", dbPasswordSecret    = { name = \"keycloak-db-secret\", key = \"password\" }"
        + "}"
    )
    $expr | save --force $tmp
    print $"  rendering deployment.dhall -> keycloak-deployment.yaml"
    dhall-to-yaml --documents --file $tmp | save --force ($output_dir | path join "keycloak-deployment.yaml")
    rm $tmp

    print $"  keycloak: done"
}
