# src/agents/build-orchestrator/package.nix
{ pkgs
, craneLib
, crateArgs
, workspaceFileset
, nix-container-lib
, inputs
, system
, ...
}:
let
  buildProcessor = craneLib.buildPackage (crateArgs // {
    cargoExtraArgs = "--bin build-processor";
    src            = workspaceFileset ./.;
    doCheck        = false;
  });

  processorContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-processor.nix;
    extraDerivations = [ buildProcessor ];
  };

in
{
  inherit buildProcessor;
  buildProcessorImage = processorContainer.image;
  cloneImage         = cloneContainer.image;
}
