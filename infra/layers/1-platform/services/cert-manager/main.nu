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

# infra/layers/1-platform/services/cert-manager/main.nu
#
# Chart entrypoint for cert-manager PKI resources.
# Renders: ca-issuer, mtls-ca certificate, leaf-issuer.
# Order matters: ca-issuer must exist before mtls-ca, mtls-ca before leaf-issuer.
#
# Called by render.nu with a context record:
#   {
#     chart_dir: string       # absolute path to this chart directory
#     output_dir: string      # absolute path to write rendered YAML into
#     overrides: string       # path to target overrides.dhall (may be empty record)
#   }
#
# Writes:
#   <output_dir>/ca-issuer.yaml
#   <output_dir>/mtls-ca.yaml
#   <output_dir>/leaf-issuer.yaml

def main [context_nuon: string] {
    let context = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir

    mkdir $output_dir

    # Render in dependency order: self-signed issuer → CA cert → leaf issuer
    for step in [
        { src: "ca-issuer.dhall",  out: "ca-issuer.yaml"  }
        { src: "mtls-ca.dhall",    out: "mtls-ca.yaml"    }
        { src: "leaf-issuer.dhall", out: "leaf-issuer.yaml" }
    ] {
        let src = ($chart_dir | path join $step.src)
        let out = ($output_dir | path join $step.out)
        print $"  rendering ($step.src) -> ($step.out)"
        dhall-to-yaml --documents --file $src | save --force $out
    }

    print $"  cert-manager: done"
}
