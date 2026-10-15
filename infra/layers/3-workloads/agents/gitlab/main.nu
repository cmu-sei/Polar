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

def main [context_nuon: string] {
    let context          = ($context_nuon | from nuon)
    let chart_dir        = $context.chart_dir
    let output_dir       = $context.output_dir
    let overrides        = $context.overrides
    let neo4j_addr       = $context.neo4jBoltAddr
    let cert_issuer_url  = $context.certIssuerUrl
    let polar_init_image = $context.polarInitImage
    let sa_audience      = $context.certIssuerAudience

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let merged      = "let v = " + $values_path + " in let o = (" + $overrides + ").gitlab in v // { imagePullSecrets = o.imagePullSecrets, observer = v.observer // o.observer, consumer = v.consumer // o.consumer }"
    let base        = "let v = (" + $merged + ") in "
    let cert_args   = ", polarInitImage = \"" + $polar_init_image + "\", certIssuerUrl = \"" + $cert_issuer_url + "\", saTokenAudience = \"" + $sa_audience + "\", proxyCACert = v.proxyCACert }"
    let neo         = ", neo4jBoltAddr = \"" + $neo4j_addr + "\""

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/observer.dhall { name = v.observer.name, image = v.observer.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets, endpoint = v.observer.endpoint, baseIntervalSecs = v.observer.baseIntervalSecs" + $cert_args
    $expr | save --force $tmp
    print $"  rendering observer.dhall -> gitlab-observer.yaml"
    dhall-to-yaml --quoted --documents --file $tmp | save --force ($output_dir | path join "gitlab-observer.yaml")
    rm $tmp

    let tmp  = (mktemp --suffix ".dhall")
    let expr = $base + $chart_dir + "/consumer.dhall { name = v.consumer.name, image = v.consumer.image, imagePullPolicy = v.imagePullPolicy, imagePullSecrets = v.imagePullSecrets" + $neo + $cert_args
    $expr | save --force $tmp
    print $"  rendering consumer.dhall -> gitlab-consumer.yaml"
    dhall-to-yaml --quoted --documents --file $tmp | save --force ($output_dir | path join "gitlab-consumer.yaml")
    rm $tmp

    print $"  gitlab: done"
}
