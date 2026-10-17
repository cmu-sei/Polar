# src/analyzers/cargo-manifest/package.nix
#
# The cargo-manifest analyzer image. The analyzer and the pipeline library are
# laid out at the same relative paths as in the repo, so analyze.nu's relative
# `use ../../../scripts/pipeline/core/...` imports resolve unchanged.
{ pkgs
, nix-container-lib
, inputs
, system
, cassiniClient
, corePipelineLibFiles
}:

let
  nuLib = pkgs.runCommand "polar-analyzer-cargo-manifest-nulib" {} ''
    mkdir -p $out/scripts/pipeline/core $out/src/analyzers/cargo-manifest
    cp ${corePipelineLibFiles}/*.nu $out/scripts/pipeline/core/
    cp ${./analyze.nu} $out/src/analyzers/cargo-manifest/analyze.nu
    cp ${./rules.nu} $out/src/analyzers/cargo-manifest/rules.nu
  '';

  # A nushell wrapper rather than writeShellApplication keeps bash out of the
  # image closure.
  entrypoint = pkgs.writers.writeNuBin "polar-analyze-cargo" ''
    ^${pkgs.nushell}/bin/nu ${nuLib}/src/analyzers/cargo-manifest/analyze.nu
  '';

  container = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container.nix;
    extraDerivations = [ entrypoint cassiniClient ];
  };
in
{
  inherit entrypoint;
  image = container.image;
}
