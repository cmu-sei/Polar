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

# infra/layers/1-platform/services/flux/main.nu
#
# Renders Flux's controllers: the generated flux-controllers.yaml (nu infra/flux.nu export), copied with each
# controller image pointed at the registry the nodes pull from. The images are mirrored there by
# infra/images.nu from the rows `flux-source-controller` and `flux-kustomize-controller` of infra/images.nuon.
#
# Renders:
#   <output_dir>/flux-controllers.yaml

def main [context_nuon: string] {
    let context = ($context_nuon | from nuon)
    let registry = (open ($context.repo_root | path join "infra" "config" "cluster.nuon") | get registry)

    mkdir $context.output_dir
    let src = ($context.chart_dir | path join "flux-controllers.yaml")
    let out = ($context.output_dir | path join "flux-controllers.yaml")
    print "  copying flux-controllers.yaml, images from the node registry"
    let text = (open --raw $src
        | lines
        | each {|l| $l | str replace --regex '^(\s+image:\s*)ghcr\.io/fluxcd/(source|kustomize)-controller:' $"${1}($registry)/flux-${2}-controller:" }
        | str join "\n")
    $"($text)\n" | save --force $out
    print "  flux: done"
}
