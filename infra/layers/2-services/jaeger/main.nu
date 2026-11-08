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

# infra/layers/2-services/jaeger/main.nu

use ../../../dhall-render.nu [dhall_yaml]

def main [context_nuon: string] {
    let context    = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir
    let overrides  = $context.overrides

    mkdir $output_dir

    let values_path = ($chart_dir | path join "values.dhall")
    let render_path = ($chart_dir | path join "deployment.dhall")
    let tmp         = (mktemp --suffix ".dhall")

    let expr = $render_path + " ((" + $values_path + ") // (" + $overrides + ").jaeger)"
    $expr | save --force $tmp

    print $"  rendering jaeger deployment + service -> jaeger.yaml"
    dhall_yaml $tmp ["--documents"] | save --force ($output_dir | path join "jaeger.yaml")
    rm $tmp

    print $"  jaeger: done"
}
