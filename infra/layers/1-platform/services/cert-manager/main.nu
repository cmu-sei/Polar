#!/usr/bin/env nu
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
