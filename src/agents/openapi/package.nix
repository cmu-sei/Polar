# src/agents/openapi/package.nix
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
  observer = craneLib.buildPackage (crateArgs // {
    pname = "openapi-observer";
    cargoExtraArgs = "--bin openapi-observer --locked";
    src = workspaceFileset ./openapi/observe;
  });

  processor = craneLib.buildPackage (crateArgs // {
    pname = "openapi-processor";
    cargoExtraArgs = "--bin openapi-processor --locked";
    src = workspaceFileset ./openapi/process;
  });

  observerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-observer.nix;
    extraDerivations = [ observer ];
  };

  processorContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-processor.nix;
    extraDerivations = [ processor ];
  };
in
{
  inherit observer processor;
  observerImage  = observerContainer.image;
  processorImage = processorContainer.image;
}
