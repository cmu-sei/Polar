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

# infra/layers/1-platform/infra/namespaces/main.nu
#
# Chart entrypoint for namespace definitions.
# Renders: polar, polar-graph, and polar-auth namespaces.
# Applied first in every target — all other resources depend on these.
#
# Called by render.nu with a context record:
#   {
#     chart_dir: string       # absolute path to this chart directory
#     output_dir: string      # absolute path to write rendered YAML into
#     overrides: string       # path to target overrides.dhall
#   }
#
# Writes:
#   <output_dir>/namespaces.yaml

def main [context_nuon: string] {
    let context   = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir

    mkdir $output_dir

    let src = ($chart_dir | path join "namespaces.dhall")
    let out = ($output_dir | path join "namespaces.yaml")
    print $"  rendering namespaces.dhall -> namespaces.yaml"
    dhall-to-yaml --documents --file $src | save --force $out

    print $"  namespaces: done"
}
