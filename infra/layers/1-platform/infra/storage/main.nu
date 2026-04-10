#!/usr/bin/env nu
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
