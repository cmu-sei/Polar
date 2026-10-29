# src/agents/polar-operator/package.nix
#
# The operator and its image. It runs as a Deployment (see src/deploy/types/operator.dhall), so it
# needs no health file or probe; it takes its arguments (`run`) from the Deployment.
{ pkgs
, craneLib
, crateArgs
, workspaceFileset
, nix-container-lib
, inputs
, system
}:

let
  operator = craneLib.buildPackage (crateArgs // {
    pname = "polar-operator";
    cargoExtraArgs = "--bin polar-operator --locked";
    src = workspaceFileset ./.;
    doCheck = false;
  });

  operatorContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container.nix;
    extraDerivations = [ operator ];
  };

in
{
  inherit operator;
  image = operatorContainer.image;
}
