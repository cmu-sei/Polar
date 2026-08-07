# src/agents/build-processor/package.nix
{ pkgs
, craneLib
, crateArgs
, workspaceFileset
, nix-container-lib
, inputs
, system
, healthcheckDrv
, ...
}:
let
  buildProcessor = craneLib.buildPackage (crateArgs // {
    cargoExtraArgs = "--bin build-processor";
    src            = workspaceFileset ./.;
    doCheck        = false;
  });

  healthcheckInputs = inputs // {
    polar-healthcheck = {
      packages.${system}.default = healthcheckDrv;
    };
  };

  processorContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = healthcheckInputs;
    configNixPath    = ./container-processor.nix;
    extraDerivations = [ buildProcessor ];
  };

in
{
  inherit buildProcessor;
  buildProcessorImage = processorContainer.image;
}
