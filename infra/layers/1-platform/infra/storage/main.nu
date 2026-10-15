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

# infra/layers/1-platform/infra/storage/main.nu
#
# Chart entrypoint for storage infrastructure.
# Renders: managed-csi StorageClass (Dhall) + local-path-provisioner (raw YAML).
#
# local-path-provisioner.yaml is raw YAML — it is copied directly to the output
# directory rather than rendered through dhall-to-yaml.
#
# Called by render.nu with a context record:
#   {
#     chart_dir: string       # absolute path to this chart directory
#     output_dir: string      # absolute path to write rendered YAML into
#     overrides: string       # path to target overrides.dhall
#   }
#
# Writes:
#   <output_dir>/storage-class.yaml
#   <output_dir>/local-path-provisioner.yaml

def main [context_nuon: string] {
    let context = ($context_nuon | from nuon)
    let chart_dir  = $context.chart_dir
    let output_dir = $context.output_dir

    mkdir $output_dir

    # Render the managed-csi StorageClass via dhall
    let src = ($chart_dir | path join "storage-class.dhall")
    let out = ($output_dir | path join "storage-class.yaml")
    print $"  rendering storage-class.dhall -> storage-class.yaml"
    dhall-to-yaml --documents --file $src | save --force $out

    # Copy the local-path-provisioner manifest verbatim
    let provisioner_src = ($chart_dir | path join "local-path-provisioner.yaml")
    let provisioner_out = ($output_dir | path join "local-path-provisioner.yaml")
    print $"  copying local-path-provisioner.yaml"
    cp $provisioner_src $provisioner_out

    print $"  storage: done"
}
